//! Running a session inside a disposable capsule (§19, §P1-037).
//!
//! ## Why the daemon does this and not the CLI
//!
//! `rime disposable run -- rime agent run …` composes the two verbs by hand
//! and is the WRONG answer, which is worth writing down because it is the
//! first thing anybody tries. The daemon spawns the session's PTY on the
//! host, and `/run/user/<uid>` is bind-mounted into every capsule — so an
//! `rime agent run` issued from inside a capsule reaches the host daemon and
//! the agent starts OUTSIDE the environment. The capsule would be an empty
//! gesture around a client that immediately left it.
//!
//! So the wrap happens where the process is actually spawned: the session's
//! PTY child becomes the disposable ENGINE, and the adapter runs inside it.
//!
//! ## What that buys, none of which is new code
//!
//! The engine (`/usr/libexec/rime-disposable`) already creates the capsule
//! with a throwaway home, copies in, and tears the whole thing down from a
//! `trap` on EXIT, INT and TERM. Making it the PTY child means:
//!
//! * teardown happens on every exit path the daemon can cause, including the
//!   SIGTERM of `rime agent kill` — the TERM trap exits 143 and the EXIT trap
//!   fires. The daemon needs no teardown hook of its own, and does not get to
//!   have a second, less tested one;
//! * the name validation, the four fences on the recursive removal, and the
//!   copy-out boundary are the engine's, unchanged.
//!
//! If the DAEMON dies mid-session the engine dies with it and its EXIT trap
//! runs; if the machine loses power, the leftover is what
//! `rime disposable list` and `rime disposable purge` exist for. That is the
//! honest bound on the guarantee and it is documented rather than papered
//! over.
//!
//! ## Where the environments live, and the one collision to know about
//!
//! The engine resolves its root from `RIME_DISPOSABLE_ROOT`, else from the
//! DAEMON's `$XDG_STATE_HOME` — normally the same directory the user's own
//! `rime disposable list` reads, because it is the same account, but it is the
//! daemon's value and not the terminal's.
//!
//! The name is derived from the session id, so it is predictable, and the
//! engine refuses to reuse a directory that already exists. Session ids start
//! again at 1 when the daemon restarts, so a leftover from a machine that lost
//! power can collide with a new session's name: the engine then exits with
//! "an environment called 'disp-agent1' already exists" on the session's own
//! terminal, and `rime disposable purge` clears it. Loud and diagnosable, and
//! preferred over a random name that no longer says which session owned it.
//!
//! ## A throwaway environment, NOT a security boundary
//!
//! distrobox mounts the host's root filesystem at `/run/host` inside every
//! capsule — that is how `distrobox-export` reaches back out to write a
//! `.desktop` file, and there is no flag that removes it — and the process
//! runs as the user's own uid. Code in a disposable capsule can therefore
//! read and write the real `$HOME`. What is disposable is the ENVIRONMENT.
//!
//! For confinement the mechanism is `policy.sandbox`, and the two are
//! REFUSED together rather than combined: bwrap wrapping the capsule engine
//! confines the container client, not the agent, so the pair would read as
//! "confined and disposable" and deliver neither.

use std::path::Path;

use anyhow::{bail, Result};
use rime_agent_core::paths;

/// The `sh -c` script that puts the adapter in the copied-in directory.
///
/// The capsule's `$HOME` is the throwaway home, and the engine copies the
/// worktree to `~/in/<basename>` inside it. `rime env exec` has no
/// working-directory flag — it is `distrobox enter --no-tty <name> -- …` — so
/// the chdir has to travel with the command.
///
/// **This string is FIXED and nothing is ever interpolated into it.** The
/// directory and the whole adapter argv arrive as POSITIONAL PARAMETERS. That
/// matters more here than it looks: the adapter's arguments carry the user's
/// prompt, which is arbitrary text, and building a shell command out of it
/// would be an injection with a friendly name. `$1` is the directory, `shift`
/// drops it, and `exec "$@"` replaces the shell so the agent is the process
/// the engine waits on — not a shell holding a child, which would swallow the
/// exit status and break the teardown's return value.
pub const CHDIR_SCRIPT: &str = r#"cd -- "$HOME/in/$1" || exit 1; shift; exec "$@""#;

/// What `--name` takes: the SUFFIX, without the `disp-` prefix.
///
/// The engine validates this half with `valid_name_suffix`, `^[a-z0-9]{1,24}$`,
/// and then prepends `disp-` itself. Passing the whole name would be refused
/// for containing a `-` — measured, and it is the reason this function exists
/// separately from [`name_for`]. See the FOUND note on the unit that added
/// this: the first version passed the full name and the feature could not
/// start at all.
///
/// `agent<id>` is inside the allowlist for every `u32` (`agent4294967295` is
/// 15 characters), and it is predictable on purpose — if teardown ever fails
/// to run, the leftover is named after the session that owned it and
/// `rime disposable list` says so.
pub fn name_suffix_for(id: u32) -> String {
    format!("agent{id}")
}

/// The disposable environment's name for session `id`, as the engine will
/// construct it and as `rime disposable list` will show it.
///
/// The engine's own rule for the whole name is `disp-[a-z0-9]{1,24}` and it is
/// a security check there, not a style one: the name becomes a container name
/// AND the final component of a path removed recursively. This is what goes in
/// [`SessionInfo::capsule`](rime_agent_core::protocol::SessionInfo::capsule) —
/// it is never what goes after `--name=`.
pub fn name_for(id: u32) -> String {
    format!("disp-{}", name_suffix_for(id))
}

/// The engine's own environment overrides, forwarded EXPLICITLY.
///
/// Both of these decide something the engine cannot be wrong about:
/// `RIME_DISPOSABLE_ROOT` is the directory it removes recursively, and
/// `RIME_DISPOSABLE_ENV_ENGINE` is the program it drives. They are forwarded
/// rather than left to inheritance because inheritance here is an ACCIDENT
/// that a correct fix elsewhere would remove:
///
/// `pty::spawn`'s `clear_env` unsets only the names it is about to set, so an
/// unconfined session's child currently inherits the daemon's whole
/// environment — 80 variables, MEASURED. Anyone who makes that path genuinely
/// default-deny, which its own doc comment claims it already is, would
/// otherwise silently point this suite's engine at the user's REAL disposable
/// root and let it delete there. Forwarding the two names makes the engine's
/// behaviour a consequence of this function instead of a consequence of that
/// bug.
///
/// Takes the lookup rather than reading the environment itself, so the unit
/// tests below are not a race against every other test in the process.
pub fn engine_env(lookup: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    ["RIME_DISPOSABLE_ROOT", "RIME_DISPOSABLE_ENV_ENGINE"]
        .iter()
        .filter_map(|name| lookup(name).map(|v| ((*name).to_string(), v)))
        .collect()
}

/// The argv that runs `program args…` inside a fresh disposable capsule.
///
/// `workdir` is COPIED into the environment, not bound. That is the whole
/// "discard state" half of this feature: the agent's edits live in the
/// capsule's throwaway home and go with it. A bind — or a path reached through
/// `/run/host` — would write to the host and discard nothing.
pub fn argv(
    id: u32,
    workdir: &Path,
    copy_out: Option<&str>,
    program: &str,
    args: &[String],
) -> Result<Vec<String>> {
    let base = workdir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty() && n != "." && n != ".." && !n.contains('/'))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} has no directory name to copy into a capsule; run from inside a project",
                workdir.display()
            )
        })?;

    let engine = paths::disposable_engine();
    let mut v = vec![
        engine.to_string_lossy().into_owned(),
        "run".to_string(),
        // The SUFFIX. The engine prepends `disp-` and refuses a name
        // containing one.
        format!("--name={}", name_suffix_for(id)),
        format!("--copy-in={}", workdir.to_string_lossy()),
    ];
    if let Some(dest) = copy_out {
        // Passed through for the ENGINE to validate. It refuses a relative
        // destination and one inside the disposable root — a destination that
        // teardown deletes would report success and produce nothing — and
        // duplicating that rule here would give the two a chance to disagree.
        v.push(format!("--copy-out={dest}"));
    }
    v.push("--".to_string());
    v.push("sh".to_string());
    v.push("-c".to_string());
    v.push(CHDIR_SCRIPT.to_string());
    // `$0` for the shell. A dash makes some shells read it as a flag.
    v.push("sh".to_string());
    v.push(base);
    v.push(program.to_string());
    v.extend(args.iter().cloned());
    Ok(v)
}

/// Refuse the combinations that would promise two things and deliver neither.
///
/// Checked before anything is created and before any password dialog, for the
/// reason the `--ttl` refusal is checked early: a caller who asked for
/// something impossible should find out in front of their own terminal. It is
/// also before `ensure_worktree`, which is what makes the `worktree` refusal
/// below leave no branch behind.
pub fn check(
    disposable: bool,
    confined: bool,
    copy_out: Option<&str>,
    worktree: bool,
    checkpoint: bool,
) -> Result<()> {
    if disposable && worktree {
        // Two failures, either one sufficient. The second is the one a user
        // would not predict:
        //
        //  * the host worktree and its branch ARE created, the capsule then
        //    copies them, and the agent's commits live in the copy — so the
        //    branch the user asked to review is left with nothing on it;
        //  * a linked worktree's `.git` is a FILE containing
        //    `gitdir: <absolute host path>`. Copied into a capsule, that path
        //    does not exist — the host root is at /run/host, not / — so git
        //    inside the capsule reports "not a git repository" and the agent
        //    has no working checkout at all.
        //
        // Not provable by the suite, and the suite says so: its fake capsule
        // engine runs on the host, where the gitdir path DOES resolve. What
        // the suite proves is the refusal.
        bail!(
            "--worktree and --disposable cannot both apply: the branch would be created on the \
             host and left empty, because the agent's commits go to the COPY inside the capsule \
             and are discarded with it. A copied linked worktree is not even a working checkout \
             — its .git is a pointer to a host path the capsule cannot reach. Pick one: \
             --worktree for work you keep on a branch, --disposable for work you throw away"
        );
    }
    if disposable && checkpoint {
        bail!(
            "--checkpoint and --disposable cannot both apply: the checkpoint would snapshot the \
             HOST tree, which a disposable session cannot change, so `rime agent undo` would \
             offer to roll back a tree this agent never touched. The capsule already discards \
             everything the agent did — that is what --disposable is"
        );
    }
    if disposable && confined {
        bail!(
            "a disposable capsule and a confining sandbox are different mechanisms and this \
             refuses to pretend they combine: the sandbox would confine the container client, \
             not the agent inside the capsule. Pick one — `--sandbox unrestricted --disposable` \
             for a throwaway ENVIRONMENT whose state is discarded, or a sandbox policy without \
             --disposable for confinement with $HOME masked"
        );
    }
    if !disposable && copy_out.is_some() {
        bail!(
            "--copy-out names where a disposable capsule's ~/out is copied when it closes, and \
             this session is not disposable; add --disposable or drop the --copy-out"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn build(workdir: &str, copy_out: Option<&str>) -> Vec<String> {
        argv(7, Path::new(workdir), copy_out, "claude", &["--foo".into()]).expect("builds")
    }

    #[test]
    fn the_worktree_is_copied_in_not_bound() {
        // The "discard state" half of the acceptance line. A bind, or a path
        // reached through /run/host, would write to the host and discard
        // nothing.
        let v = build("/home/u/proj", None);
        assert!(
            v.contains(&"--copy-in=/home/u/proj".to_string()),
            "{v:?}"
        );
        assert!(
            !v.iter().any(|a| a.contains("/run/host")),
            "nothing reaches the host filesystem by path: {v:?}"
        );
    }

    #[test]
    fn nothing_leaves_unless_copy_out_is_asked_for() {
        let v = build("/home/u/proj", None);
        assert!(!v.iter().any(|a| a.starts_with("--copy-out")), "{v:?}");
        let v = build("/home/u/proj", Some("/home/u/results"));
        assert!(v.contains(&"--copy-out=/home/u/results".to_string()), "{v:?}");
    }

    #[test]
    fn the_adapter_argv_arrives_as_positional_parameters() {
        // The injection this design exists to make impossible. The adapter's
        // arguments carry the user's prompt — arbitrary text — and the shell
        // script must never be built out of it.
        let v = argv(
            1,
            Path::new("/home/u/proj"),
            None,
            "claude",
            &["; rm -rf ~".to_string(), "$(id)".to_string()],
        )
        .expect("builds");
        let script = v.iter().position(|a| a == CHDIR_SCRIPT).expect("the script");
        assert_eq!(v[script - 1], "-c");
        assert_eq!(v[script - 2], "sh");
        // Everything hostile is a LATER, separate argument. The script itself
        // is byte-for-byte the constant.
        assert_eq!(v[script], CHDIR_SCRIPT);
        assert!(v.contains(&"; rm -rf ~".to_string()));
        assert!(v.contains(&"$(id)".to_string()));
        assert!(
            !CHDIR_SCRIPT.contains("rm"),
            "the script is a constant and cannot carry an argument"
        );
    }

    #[test]
    fn the_command_starts_in_the_copied_in_directory() {
        let v = build("/home/u/my-repo", None);
        let script = v.iter().position(|a| a == CHDIR_SCRIPT).expect("the script");
        // $0, then $1 — the basename the engine copies to ~/in/<basename>.
        assert_eq!(v[script + 1], "sh");
        assert_eq!(v[script + 2], "my-repo");
        assert_eq!(v[script + 3], "claude");
        assert_eq!(v[script + 4], "--foo");
        assert!(CHDIR_SCRIPT.contains("$HOME/in/$1"));
        assert!(
            CHDIR_SCRIPT.contains("exec"),
            "exec, so the agent is the process the engine waits on"
        );
    }

    #[test]
    fn the_separator_before_the_command_is_not_optional() {
        // Without `--` the engine reads `sh` as one of its own arguments.
        let v = build("/home/u/proj", None);
        let sep = v.iter().position(|a| a == "--").expect("a separator");
        assert_eq!(v[sep + 1], "sh");
        assert!(v[..sep].iter().all(|a| a.starts_with("--") || !a.is_empty()));
    }

    #[test]
    fn the_name_passed_to_the_engine_is_the_suffix_it_validates() {
        // THE assertion this file needs, and the one its first version got
        // wrong: `--name` takes the SUFFIX. The engine checks it against
        // `valid_name_suffix`, `^[a-z0-9]{1,24}$`, and prepends `disp-`
        // itself — so a value containing the prefix is refused for containing
        // a `-`, and every disposable session died at the engine's argument
        // parse. Checking `name_for` against the FULL-name rule passed while
        // that was true, which is why this test reads what is actually pushed.
        for id in [0u32, 7, 1_000, u32::MAX] {
            let v = argv(id, Path::new("/home/u/proj"), None, "claude", &[]).expect("builds");
            let flag = v
                .iter()
                .find(|a| a.starts_with("--name="))
                .expect("a --name")
                .strip_prefix("--name=")
                .unwrap()
                .to_string();

            // valid_name_suffix: no prefix, no separator, and short enough.
            assert!(!flag.contains('-'), "the engine refuses a '-' here: {flag}");
            assert!(!flag.starts_with("disp"), "the engine adds the prefix: {flag}");
            assert!(!flag.is_empty() && flag.len() <= 24, "{flag}");
            assert!(
                flag.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                "{flag}"
            );

            // ...and what the engine will BUILD from it is the name recorded
            // on the session, so `rime disposable list` and `rime agent
            // status` say the same word. valid_disposable_name: <= 29.
            assert_eq!(name_for(id), format!("disp-{flag}"));
            assert!(name_for(id).len() <= 29, "{}", name_for(id));
        }
        assert_eq!(name_suffix_for(7), "agent7");
        assert_eq!(name_for(7), "disp-agent7");
    }

    #[test]
    fn the_engines_root_and_capsule_engine_are_forwarded_not_left_to_inheritance() {
        // The variable that decides which directory is removed recursively.
        // Inheritance currently carries it (pty::spawn's clear_env unsets only
        // the names it sets), but that is the bug its own doc comment denies,
        // and a fix would otherwise point a test suite's engine at the user's
        // real disposable root.
        let env = engine_env(|name| match name {
            "RIME_DISPOSABLE_ROOT" => Some("/tmp/fixture/disp".to_string()),
            "RIME_DISPOSABLE_ENV_ENGINE" => Some("/tmp/fixture/fake-env".to_string()),
            _ => None,
        });
        assert_eq!(
            env,
            vec![
                ("RIME_DISPOSABLE_ROOT".to_string(), "/tmp/fixture/disp".to_string()),
                (
                    "RIME_DISPOSABLE_ENV_ENGINE".to_string(),
                    "/tmp/fixture/fake-env".to_string()
                ),
            ]
        );
        // Unset stays unset: production sets neither, and forwarding an empty
        // value would override the engine's own default with nothing.
        assert!(engine_env(|_| None).is_empty());
    }

    #[test]
    fn a_directory_with_no_name_is_refused_rather_than_guessed() {
        // `/` has no basename, so there is nothing for the engine to copy to
        // ~/in/<name>. Refusing here beats letting the engine fail after it
        // has created an environment.
        assert!(argv(1, Path::new("/"), None, "claude", &[]).is_err());
    }

    #[test]
    fn a_confining_sandbox_and_a_disposable_capsule_are_refused_together() {
        // The important refusal. bwrap wrapping the capsule engine confines
        // the container client and not the agent, so the pair would read as
        // "confined AND disposable" and deliver neither.
        let err = check(true, true, None, false, false).expect_err("must refuse");
        let text = format!("{err:#}");
        assert!(text.contains("different mechanisms"), "{text}");
        assert!(text.contains("not the agent"), "{text}");

        // ...and each alone is fine.
        assert!(check(true, false, None, false, false).is_ok());
        assert!(check(false, true, None, false, false).is_ok());
    }

    #[test]
    fn copy_out_without_a_capsule_is_refused_not_ignored() {
        // A caller who named a destination believes they asked for something.
        // clap refuses this at the CLI too (`requires = "disposable"`); this
        // is the arm that answers every OTHER client of the socket, and it is
        // the only thing watching it — the shell suite exercises clap's.
        let err = check(false, false, Some("/home/u/out"), false, false).expect_err("must refuse");
        assert!(format!("{err:#}").contains("not disposable"));
        assert!(check(true, false, Some("/home/u/out"), false, false).is_ok());
    }

    #[test]
    fn a_worktree_and_a_capsule_are_refused_together() {
        // The branch would be created on the host and left empty, because the
        // commits go to the copy. And a copied linked worktree is not a
        // checkout at all: its .git is a pointer to a host path.
        let err = check(true, false, None, true, false).expect_err("must refuse");
        let text = format!("{err:#}");
        assert!(text.contains("left empty"), "{text}");
        assert!(text.contains("pointer to a host path"), "{text}");
        // A worktree WITHOUT a capsule is the ordinary case and must be fine.
        assert!(check(false, false, None, true, false).is_ok());
    }

    #[test]
    fn a_checkpoint_and_a_capsule_are_refused_together() {
        // A checkpoint of a tree the session cannot change would let
        // `rime agent undo` offer to roll back work this agent never did.
        let err = check(true, false, None, false, true).expect_err("must refuse");
        assert!(format!("{err:#}").contains("never touched"));
        assert!(check(false, false, None, false, true).is_ok());
    }

    #[test]
    fn the_engine_path_is_overridable_for_a_suite_that_must_not_make_capsules() {
        // Not a convenience: without it no test can reach the engine in the
        // repository, and every assertion below would need podman and an
        // image pull.
        assert_eq!(
            PathBuf::from(paths::DISPOSABLE_ENGINE),
            PathBuf::from(rime_paths::RIME_DISPOSABLE)
        );
        assert_eq!(paths::DISPOSABLE_ENGINE_ENV, "RIME_DISPOSABLE_ENGINE");
    }
}
