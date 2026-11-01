//! `rime git-shim` — the `git` a managed session finds first on its PATH.
//!
//! §12's constraint is that the user's existing skills keep using normal tools:
//! a skill runs `git push`, and it works, and nobody rewrites it around
//! `rime secret use github git-push origin`. This is how.
//!
//! The daemon writes a two-line `git` into the session's scratch directory and
//! puts that directory first on the session's `PATH`. It runs this. For the
//! three operations the broker has a capability for — `push`, `fetch`,
//! `ls-remote` — against a remote whose URL is `http(s)` and whose host has a
//! stored credential, it asks the broker to perform the operation. For
//! everything else it `exec`s the real git and gets out of the way.
//!
//! ## What it is not
//!
//! **Not a security boundary.** It holds no credential and enforces nothing.
//! The agent can run `/usr/bin/git` directly, and the only thing that happens
//! is that git has no credential and the remote answers `401`. Every check that
//! matters is in `rime-secretd`: the grant, the host pin, the URL resolved from
//! the repository rather than from the caller. This is a convenience, and it is
//! important to say so, because a shim that looked like enforcement would be
//! the kind of thing somebody later relies on.
//!
//! **Not a translation layer.** Anything the capability model does not express
//! falls through to git rather than being approximated: a refspec, a `--force`,
//! a `clone` of a URL. The capability vocabulary is closed on purpose, and the
//! wrong way to widen it is one argument at a time inside a wrapper.
//!
//! ## Falling through, and when not to
//!
//! An ssh remote, a public repository, a remote on a host with no stored
//! credential: all of those work today without a credential, and a shim that
//! turned them into "not granted" would break more than it fixed. So the
//! broker's `NoSuchService`, `HostMismatch` and "not an http remote" answers
//! all fall through to real git.
//!
//! A refusal does not. "This capability is not granted for this project" is the
//! answer the person needs; running git afterwards would replace it with git's
//! own authentication failure, which says nothing about grants and sends the
//! reader looking in the wrong place.

use std::os::unix::process::CommandExt;
use std::process::Command;

use rime_agent_core::protocol::{Request as AgentRequest, Response as AgentResponse};

/// The real git, by absolute path.
///
/// Never a `PATH` lookup: this program is itself on the `PATH` as `git`, so a
/// lookup would find it again and recurse until the session runs out of
/// processes.
const REAL_GIT: &str = rime_paths::GIT;

/// The git subcommands the broker has a capability for.
///
/// §13.2's ids, not P0-002's `git-push` spelling. The daemon's registry
/// canonicalises the old names too, so either would work — this sends the one
/// that ends up in the grant table and the audit trail, so the shim's requests
/// read the same as a hand-typed `rime secret use`.
fn capability_for(subcommand: &str) -> Option<&'static str> {
    match subcommand {
        "push" => Some("git.push"),
        "fetch" => Some("git.fetch"),
        "ls-remote" => Some("git.ls-remote"),
        _ => None,
    }
}

/// What the shim decided to do with an argument list.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Run the real git with these arguments unchanged.
    PassThrough,
    /// Ask the broker to perform `capability` on `remote`, with `branch` for a
    /// push that named one.
    Broker {
        capability: &'static str,
        remote: String,
        branch: Option<String>,
    },
}

/// Read an argument list the way git would, as far as this needs to.
///
/// Deliberately conservative: anything with a shape this does not fully
/// understand is [`Plan::PassThrough`], because passing through is always
/// correct and brokering the wrong thing is not. In particular every flag on
/// the operation itself falls through — `--force`, `--tags`, `--prune`,
/// `--depth` — since the capability model expresses none of them and quietly
/// dropping one would be a `git push --force` that did not force.
pub fn plan(args: &[String]) -> Plan {
    let mut rest = args.iter();
    let mut subcommand = None;
    // Git's own global options come before the subcommand. Only the ones that
    // take no value are skipped; anything else falls through, because
    // mis-reading `-c core.x=y` as a subcommand is how a shim brokers the
    // wrong operation.
    for arg in rest.by_ref() {
        if arg == "--no-pager" || arg == "--paginate" || arg == "-p" {
            continue;
        }
        if arg.starts_with('-') {
            return Plan::PassThrough;
        }
        subcommand = Some(arg.as_str());
        break;
    }
    let Some(subcommand) = subcommand else {
        return Plan::PassThrough;
    };
    let Some(capability) = capability_for(subcommand) else {
        return Plan::PassThrough;
    };

    let mut positional: Vec<&String> = Vec::new();
    for arg in rest {
        if arg == "--" {
            return Plan::PassThrough;
        }
        if arg.starts_with('-') {
            return Plan::PassThrough;
        }
        positional.push(arg);
    }

    // A remote given as a URL is not a name the broker will take, and it is not
    // one this should turn into a name either — the whole point of naming a
    // remote is that the daemon resolves it from the repository.
    let remote = match positional.first() {
        Some(first) if looks_like_a_url(first) => return Plan::PassThrough,
        Some(first) => (*first).clone(),
        None => "origin".to_string(),
    };

    let branch = match (capability, positional.get(1)) {
        // A push refspec — `HEAD:main`, `+main`, `refs/heads/x` — is not a
        // branch name and the capability has nowhere to put one.
        ("git.push", Some(second)) if is_refspec(second) => return Plan::PassThrough,
        ("git.push", Some(second)) => Some((*second).clone()),
        // A fetch or ls-remote with a second positional is naming refs, which
        // the capability does not express.
        (_, Some(_)) => return Plan::PassThrough,
        (_, None) => None,
    };
    if positional.len() > 2 {
        return Plan::PassThrough;
    }

    Plan::Broker {
        capability,
        remote,
        branch,
    }
}

fn looks_like_a_url(arg: &str) -> bool {
    arg.contains("://") || arg.contains(':') || arg.contains('/')
}

fn is_refspec(arg: &str) -> bool {
    arg.contains(':') || arg.starts_with('+') || arg.starts_with("refs/")
}

pub fn main(args: Vec<String>) -> i32 {
    match plan(&args) {
        Plan::PassThrough => pass_through(&args),
        Plan::Broker {
            capability,
            remote,
            branch,
        } => broker(capability, &remote, branch.as_deref(), &args),
    }
}

/// Become the real git.
///
/// `exec`, not spawn-and-wait: the caller is watching this process's exit
/// status and its terminal, and an extra process in between changes how signals
/// and job control reach git.
fn pass_through(args: &[String]) -> i32 {
    let error = Command::new(REAL_GIT).args(args).exec();
    eprintln!("rime: cannot run {REAL_GIT}: {error}");
    127
}

fn broker(capability: &str, remote: &str, branch: Option<&str>, args: &[String]) -> i32 {
    let project = std::env::current_dir()
        .ok()
        .and_then(|cwd| rime_agent_core::project::detect(&cwd))
        .map(|p| p.root);

    let mut agent = match rime_agent_core::client::Client::connect() {
        Ok(c) => c,
        // No runtime, no broker, and this is a convenience: a session started
        // outside the agent runtime gets plain git rather than an error about
        // a daemon it never asked for.
        Err(_) => return pass_through(args),
    };

    // A branch is one of `git.push`'s declared options, so it travels as a
    // parameter rather than as a field of its own. That is the whole of what
    // P1-001 changed here: the shim names an operation and its arguments, and
    // knows nothing about which provider serves them.
    let mut params = std::collections::BTreeMap::new();
    if let Some(branch) = branch {
        params.insert("branch".to_string(), branch.to_string());
    }
    let reply = agent.request(&AgentRequest::SecretUse {
        service: service_for(remote),
        operation: capability.to_string(),
        resource: remote.to_string(),
        params,
        body: None,
        project,
    });

    match reply {
        Ok(AgentResponse::Brokered {
            endpoint,
            exit_code,
            output,
            ..
        }) => {
            if !output.trim().is_empty() {
                println!("{}", output.trim_end());
            }
            if exit_code != 0 {
                eprintln!("rime: brokered {capability} against {endpoint} exited {exit_code}");
            }
            exit_code
        }
        Ok(AgentResponse::Error { message, .. }) => {
            if falls_through(&message) {
                return pass_through(args);
            }
            eprintln!("rime: {message}");
            1
        }
        Ok(_) | Err(_) => pass_through(args),
    }
}

/// Whether a refusal means "this is not a brokered remote" rather than "you may
/// not do that".
///
/// Matched on the message because the two protocols share one error kind for
/// both, and the distinction matters more than the coupling costs: a wrong
/// answer in the falls-through direction runs git and git says what is wrong,
/// while a wrong answer the other way replaces "not granted" with an
/// authentication failure that sends the reader somewhere else entirely.
fn falls_through(message: &str) -> bool {
    [
        "no credential stored",
        "is not an http remote",
        "but this credential is for",
        "has no remote called",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

/// The stored credential a remote is served by.
///
/// One name for now — `github` — because that is the credential this machine
/// has, and a shim that guessed service names would fail in a way nobody could
/// debug. When the broker answers "no credential stored for github", the shim
/// falls through and git behaves as it always did.
fn service_for(_remote: &str) -> String {
    std::env::var("RIME_GIT_SERVICE").unwrap_or_else(|_| "github".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn the_three_brokered_operations_are_recognised() {
        assert_eq!(
            plan(&argv("push")),
            Plan::Broker {
                capability: "git.push",
                remote: "origin".into(),
                branch: None
            }
        );
        assert_eq!(
            plan(&argv("push upstream my-branch")),
            Plan::Broker {
                capability: "git.push",
                remote: "upstream".into(),
                branch: Some("my-branch".into())
            }
        );
        assert_eq!(
            plan(&argv("fetch origin")),
            Plan::Broker {
                capability: "git.fetch",
                remote: "origin".into(),
                branch: None
            }
        );
        assert_eq!(
            plan(&argv("ls-remote")),
            Plan::Broker {
                capability: "git.ls-remote",
                remote: "origin".into(),
                branch: None
            }
        );
    }

    #[test]
    fn everything_the_capability_model_cannot_express_falls_through() {
        // Each of these has a correct answer that the broker cannot give, and
        // approximating one would be worse than not intercepting: a dropped
        // `--force` is a push that silently did not force.
        for line in [
            "push --force",
            "push --force-with-lease origin main",
            "push --tags",
            "push origin HEAD:main",
            "push origin +main",
            "push origin refs/heads/x",
            "fetch --all",
            "fetch --depth 1 origin",
            "fetch origin main",
            "ls-remote --heads origin",
            "push https://example.com/r.git",
            "push git@github.com:a/b",
            "push origin main extra",
            "push -- origin",
        ] {
            assert_eq!(plan(&argv(line)), Plan::PassThrough, "git {line}");
        }
    }

    #[test]
    fn every_other_subcommand_falls_through() {
        // The shim exists for three operations. Everything else is the whole of
        // git, and touching any of it would be a wrapper nobody asked for.
        for line in [
            "status", "commit -m x", "log", "clone https://x/y", "pull", "add .", "rebase -i",
            "worktree add /tmp/w", "diff", "",
        ] {
            assert_eq!(plan(&argv(line)), Plan::PassThrough, "git {line}");
        }
    }

    #[test]
    fn a_global_option_before_the_subcommand_falls_through() {
        // `-c`, `-C` and `--git-dir` take a value, and reading that value as
        // the subcommand is how a shim brokers an operation nobody asked for.
        for line in [
            "-c core.hooksPath=/x push",
            "-C /other/repo push",
            "--git-dir=/x fetch",
            "--exec-path push",
        ] {
            assert_eq!(plan(&argv(line)), Plan::PassThrough, "git {line}");
        }
        // The two that take no value are skipped, because `git --no-pager push`
        // is a push.
        assert!(matches!(plan(&argv("--no-pager push")), Plan::Broker { .. }));
    }

    #[test]
    fn a_refusal_about_the_remote_falls_through_and_one_about_a_grant_does_not() {
        // The distinction the shim exists to get right. An ssh remote or a
        // public host must keep working exactly as it did; a missing grant must
        // be reported as a missing grant and not as git failing to
        // authenticate.
        for message in [
            "no credential stored for 'github'; add one with `rime secret add github`",
            "git@github.com:a/b is not an http remote, so a stored token is not how it authenticates",
            "that remote points at gitlab.com, but this credential is for github.com",
            "this repository has no remote called 'upstream'",
        ] {
            assert!(falls_through(message), "{message}");
        }
        for message in [
            "'git-push' on 'github' is not granted for this project",
            "this session was started with the secret capability layer off",
            "this session is not inside a project, so no capability can be granted to it",
        ] {
            assert!(!falls_through(message), "{message}");
        }
    }

    #[test]
    fn the_real_git_is_an_absolute_path() {
        // A PATH lookup would find this program, which is installed on the
        // session's PATH as `git`, and recurse.
        assert!(std::path::Path::new(REAL_GIT).is_absolute());
    }
}
