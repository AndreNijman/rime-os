//! APEX protected secret service, shared library (roadmap §11, task P0-002).
//!
//! §3.2: *"Normal managed agents should not receive raw long-lived secrets
//! where APEX can broker the operation instead."* §11 asks for that brokering
//! to live in a dedicated service, separate from both `apex-agentd` and broad
//! `apexd`. This crate is the model that service and its clients share.
//!
//! ## The flow
//!
//! ```text
//! agent / tool
//!   → capability request        CapabilityRecord, built by apex-agentd
//!   → policy decision           the session's secret dimension, then the grant
//!   → broker-owned operation    apex-secretd runs it; the value stays here
//!   → provider
//!   → result                    output, with the credential scrubbed out
//! ```
//!
//! ## Why a separate daemon, and what that buys
//!
//! `apex-agentd` runs as the user, because it launches the user's own programs.
//! Anything it can read, a managed agent with that uid can read too — including
//! an unrestricted session, which by definition has the whole home. The old
//! broker kept credentials in `$XDG_STATE_HOME`, `0600`, which stops another
//! *account* and nothing else.
//!
//! `apex-secretd` runs as root with its store at `/var/lib/apex-secretd`,
//! `0700`. A process with the user's uid cannot read a credential at rest, and
//! the API has no verb that returns one. The daemon performs the operation
//! itself and returns the operation's output.
//!
//! ## What this does not protect against
//!
//! Written here rather than in a release note, because a boundary whose limits
//! are not stated gets trusted for things it never did:
//!
//! * **A same-uid process can still ask for any operation the owner granted.**
//!   Session identity comes from `apex-agentd`, which runs as the user, so it
//!   is attribution and not authentication. What a same-uid attacker gets is
//!   the *use* of a granted capability, never the credential.
//! * **The credential is in the environment of the `git` child while it runs**,
//!   and that child runs as the owner so the operation can touch the owner's
//!   repository. A same-uid process outside the sandbox can read
//!   `/proc/<pid>/environ` during those milliseconds. A *confined* session
//!   cannot: the sandbox uses `--unshare-pid`, so the daemon's children are not
//!   in the agent's `/proc` at all.
//! * **A repository is caller-controlled** and git reads its local config. The
//!   broker disables the hook and helper settings it can reach from the command
//!   line (see `apex-secretd`'s `git` module), which is a mitigation and not a
//!   proof.
//! * **Root compromise ends the discussion**, here as everywhere.
//!
//! The two properties it does hold, and which P0-002 exists for: a credential
//! is not readable at rest by the user's uid, and no reply this service can
//! send contains one.

pub mod audit;
pub mod budget;
pub mod capability;
pub mod client;
pub mod identity;
pub mod operation;
pub mod paths;
pub mod project;
pub mod protocol;
pub mod store;
pub mod value;

pub use audit::{AuditEvent, AuditLine};
pub use budget::{Budget, BudgetError, Spend, Usage};
pub use capability::{CapabilityRecord, EndpointError};
pub use operation::{
    Effect, OperationId, OperationInfo, OperationSpec, ParamInfo, ParamSpec, Params, ProviderSpec,
    ResourceKind, Syntax, VocabularyError,
};
pub use project::{ProjectConfig, ProjectError};
pub use protocol::{ErrorKind, Request, Response};
pub use store::{Grants, ServiceInfo, Store, StoreError};
pub use value::SecretValue;
