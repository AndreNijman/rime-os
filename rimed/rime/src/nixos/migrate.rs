//! `rime migrate` on Rime OS on NixOS: `finish`, the reclaim step of the
//! move from bootc (docs/nixos/bridge.md "rime migrate finish").
//!
//! The bridge's other verbs (`check`, `repair-e`, `key-export`, `start`) run
//! on the old (bootc) system, where this CLI execs `/usr/libexec/rime-migrate`
//! with the arguments it was given (the readiness release, rime-os main).
//! On NixOS only `finish` exists: it execs the helper the rime.rootfs module
//! installs on a migrated machine (nix/pkgs/rime-bridge/rime-migrate-finish.py),
//! by its `/run/current-system/sw` path. Every other verb is refused by name,
//! with where it runs. Like every engine path here, no program path comes from
//! the environment.

use std::os::unix::process::CommandExt;
use std::process::Command;

/// `rime migrate finish` on a migrated NixOS machine (nix/modules/rootfs.nix).
pub const FINISH: &str = "/run/current-system/sw/bin/rime-migrate-finish";
/// The bridge on the old (bootc) system (rime-os main, `nixos_bridge::MIGRATE_ENGINE`).
pub const BOOTC_ENGINE: &str = rime_paths::RIME_MIGRATE;

/// What `rime migrate ARGS` does on this system.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    /// exec this program with these arguments
    Exec(&'static str, Vec<String>),
    /// print this (stdout) and exit 0
    Usage(String),
    /// print this (stderr) and exit 2
    Refused(String),
}

pub fn route(args: &[String], nixos: bool) -> Route {
    let verb = args.first().map(String::as_str);
    match (nixos, verb) {
        (true, Some("finish")) => Route::Exec(FINISH, args[1..].to_vec()),
        (true, None | Some("-h" | "--help" | "help")) => Route::Usage(
            "rime migrate finish [--dry-run | --yes]\n\
             \x20 Removes the old (bootc) system this machine was moved from: the GRUB menu's ostree\n\
             \x20 entries, @pre-nixos and ostree/ (the old home with it). --dry-run (the default) writes\n\
             \x20 nothing and says what --yes would delete, the space it should free and what stops working.\n\
             \x20 Needs three good NixOS boots since the move.\n"
                .to_string(),
        ),
        (true, Some(v)) => Route::Refused(format!(
            "rime: `rime migrate {v}` runs on the old (bootc) system, before the move; \
             this is Rime OS on NixOS, where only `rime migrate finish` is left to do"
        )),
        (false, Some("finish")) => Route::Refused(
            "rime: `rime migrate finish` runs on Rime OS on NixOS, after `rime migrate start` \
             (boot the Rime entry in the GRUB menu)"
                .to_string(),
        ),
        (false, _) => Route::Exec(BOOTC_ENGINE, args.to_vec()),
    }
}

/// Not root-gated here: finish refuses a non-root run itself, with the sudo
/// line to type, after argument parsing (so `--help` works).
pub fn run(args: &[String]) -> i32 {
    match route(args, super::is_nixos()) {
        Route::Exec(prog, a) => {
            let err = Command::new(prog).args(&a).exec();
            eprintln!("rime: cannot run {prog}: {err}");
            1
        }
        Route::Usage(text) => {
            print!("{text}");
            0
        }
        Route::Refused(text) => {
            eprintln!("{text}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn finish_on_nixos_execs_the_helper_with_the_rest() {
        assert_eq!(
            route(&v(&["finish", "--yes"]), true),
            Route::Exec(FINISH, v(&["--yes"]))
        );
        assert_eq!(route(&v(&["finish"]), true), Route::Exec(FINISH, vec![]));
        assert_eq!(FINISH, "/run/current-system/sw/bin/rime-migrate-finish");
    }

    #[test]
    fn bootc_verbs_on_nixos_are_refused_by_name() {
        for verb in ["check", "start", "repair-e", "key-export"] {
            match route(&v(&[verb, "--explain"]), true) {
                Route::Refused(t) => assert!(t.contains(&format!("rime migrate {verb}")), "{t}"),
                r => panic!("{verb}: {r:?}"),
            }
        }
        assert!(matches!(route(&[], true), Route::Usage(_)));
    }

    #[test]
    fn bootc_side_unchanged_and_finish_refused_there() {
        assert_eq!(
            route(&v(&["check", "--explain"]), false),
            Route::Exec(BOOTC_ENGINE, v(&["check", "--explain"]))
        );
        assert!(matches!(route(&v(&["finish"]), false), Route::Refused(_)));
    }
}
