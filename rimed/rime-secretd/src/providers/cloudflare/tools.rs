//! P1-012: `wrangler` and `terraform`, run by the broker, credentialled by it.
//!
//! > *"Existing skills can continue invoking normal tools."*
//! > *"Tool wrappers/helpers request capabilities transparently."*
//!
//! ## What is transparent about it
//!
//! A skill that has always run `wrangler deploy` goes on running
//! `wrangler deploy`. What changes is which `wrangler` is first on its `PATH`:
//! `files/system/libexec/rime-tool-broker`, symlinked as `wrangler` and
//! `terraform`, which recognises the subcommands below and asks
//! `rime-secretd` to run the real one. The tool believes it has credentials
//! because it does — in its own environment, put there by a process the agent
//! cannot read. The agent never had them.
//!
//! Anything the shim does not recognise execs the real tool unchanged, so
//! `wrangler --version` and `wrangler types` keep working. They will not be
//! authenticated, which is the truth: the agent has no credential.
//!
//! ## Why the vocabulary is subcommands and not "run wrangler"
//!
//! An operation called `cloudflare.wrangler.run` taking the caller's own argv
//! would be a grant to do anything `wrangler` can do, which is everything this
//! provider spent P1-004 through P1-010 carefully not granting in one piece.
//! It would also be ungovernable: `wrangler delete` and `wrangler deploy` are
//! one word apart. So each brokered subcommand is its own operation with its
//! own grant and a **fixed argv this file writes**. Nothing the caller sends
//! reaches the command line except through [`Brokered::environment`], which is
//! an environment name the project's own `rime.toml` had to bind first.
//!
//! ## Why the list is short, and why `cloudflared` is not on it
//!
//! [`crate::provider::Performed`] carries an exit code and a `String`. A tool
//! that streams or blocks does not fit it and would hang a connection thread
//! until the timeout: `wrangler tail`, `terraform apply` without
//! `-auto-approve`, and every useful `cloudflared` subcommand are all one of
//! those. `cloudflared tunnel run` *is* the streaming case — it is a daemon —
//! and `mod.rs` already declines to broker a connector token for a reason that
//! has not changed: the token is presented to the Cloudflare edge over QUIC,
//! not to any HTTPS host the store can pin it to. §12's `cloudflared auth ->
//! Rime` is where that belongs, and it is not this.
//!
//! So: four subcommands, each of which starts, finishes and prints.
//!
//! ## What §13.1's binding gates here, and what it does not
//!
//! Worth stating plainly, because it is weaker than it looks. For the REST
//! operations the project's `rime.toml` decides the **script name in the URL**,
//! so a worker the project did not bind cannot be addressed at all. For a
//! brokered `wrangler`, the binding decides the grant, the account id and
//! `--env` — but the script that gets deployed is whatever `wrangler.toml` in
//! the caller's own directory names. `wrangler` reads that file and this build
//! does not.
//!
//! That is the same trust model rather than a hole: `rime.toml` and
//! `wrangler.toml` are both the caller's own files, in the caller's own
//! project, and a caller who can edit one can edit the other. But it means
//! `a_worker_this_project_did_not_bind_never_reaches_wrangler` proves the
//! *name* was refused before anything ran, not that wrangler could only have
//! deployed the bound script.

use std::path::{Path, PathBuf};

/// Which tool, and where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    Wrangler,
    Terraform,
}

impl Program {
    pub fn binary(&self) -> &'static str {
        match self {
            Program::Wrangler => "wrangler",
            Program::Terraform => "terraform",
        }
    }
}

/// Where the tools are, so a test can put its own there.
///
/// A field on the provider rather than a constant, for exactly the reason
/// [`super::api::Api`] is one: there is no `wrangler` on the machine this was
/// built on, and a build that could only run the real one could not be tested
/// at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools {
    dir: PathBuf,
}

impl Tools {
    /// Where a system install puts them. `/usr/local/bin` first because that
    /// is `/var/usrlocal` on an ostree system and is where an owner's own
    /// install lands; `/usr/bin` is the image's.
    pub fn system() -> Tools {
        Tools {
            dir: PathBuf::new(),
        }
    }

    /// One directory holding both, for a test.
    #[cfg(test)]
    pub fn in_dir(dir: &Path) -> Tools {
        Tools {
            dir: dir.to_path_buf(),
        }
    }

    /// The absolute path this build will run, or nothing if it is not there.
    ///
    /// Absent is answered as absent. A tool that is not installed is not a
    /// tool the caller may not use, and the two have different fixes — which
    /// is the same distinction this provider keeps everywhere else.
    pub fn path(&self, program: Program) -> Option<PathBuf> {
        if !self.dir.as_os_str().is_empty() {
            let candidate = self.dir.join(program.binary());
            return candidate.exists().then_some(candidate);
        }
        [rime_paths::LOCAL_BIN_DIR, rime_paths::SYSTEM_BIN_DIR]
            .iter()
            .map(|dir| Path::new(dir).join(program.binary()))
            .find(|path| path.exists())
    }
}

/// One brokered subcommand.
pub struct Brokered {
    pub program: Program,
    /// The argv, written here and nowhere else.
    pub args: &'static [&'static str],
    /// Whether `--env <name>` is appended for a worker bound to an
    /// environment. Only wrangler has the notion.
    pub environment: bool,
}

/// The four subcommands this build will run, and nothing else.
///
/// Adding one means adding a row here, an [`super::SPEC`] entry, a
/// [`super::temporary::POLICY`] row and a grant name — four places, on
/// purpose. A tool subcommand that can be reached without all four is one
/// nobody decided to allow.
pub const BROKERED: &[(&str, Brokered)] = &[
    (
        "cloudflare.wrangler.deploy",
        Brokered {
            program: Program::Wrangler,
            args: &["deploy"],
            environment: true,
        },
    ),
    (
        "cloudflare.wrangler.versions-upload",
        Brokered {
            program: Program::Wrangler,
            // §13.7's first step: a version exists but no traffic reaches it.
            args: &["versions", "upload"],
            environment: true,
        },
    ),
    (
        "cloudflare.terraform.plan",
        Brokered {
            program: Program::Terraform,
            // `-input=false` because there is no terminal to prompt at, and a
            // terraform that decides to ask a question would sit there until
            // the timeout killed it.
            args: &["plan", "-input=false", "-no-color"],
            environment: false,
        },
    ),
    (
        "cloudflare.terraform.apply",
        Brokered {
            program: Program::Terraform,
            // `-auto-approve` is not this build being cavalier: the approval
            // already happened, at the grant, which is where Rime puts it.
            // Terraform's own prompt cannot be answered from here, and a
            // brokered apply that blocked on one would be a hung connection
            // rather than a safeguard.
            args: &["apply", "-input=false", "-no-color", "-auto-approve"],
            environment: false,
        },
    ),
];

/// The brokered subcommand for an operation id, if it is one.
pub fn brokered(operation: &str) -> Option<&'static Brokered> {
    BROKERED
        .iter()
        .find(|(id, _)| *id == operation)
        .map(|(_, b)| b)
}

/// The variable each tool reads its Cloudflare credential from.
///
/// The same one for both, which is not a coincidence: `terraform`'s Cloudflare
/// provider reads `CLOUDFLARE_API_TOKEN` because that is what `wrangler` made
/// conventional.
pub const CREDENTIAL_VARIABLE: &str = "CLOUDFLARE_API_TOKEN";
