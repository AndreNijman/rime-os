//! Rime agent runtime, shared library.
//!
//! Everything the runtime knows that is not I/O: the control protocol, the
//! sandbox policy, the adapters, project detection and checkpoints. The daemon
//! (`rime-agentd`) and the CLI (`rime agent`) both build on this, so the two
//! can never disagree about what a session is or what a policy means.
//!
//! The split is also what makes the security-relevant parts testable. The
//! sandbox argv, the state machine and the checkpoint plumbing are pure
//! functions over their inputs, asserted directly rather than through a live
//! PTY — and they are the shipped functions, not a reimplementation of them.
//!
//! ## Architecture
//!
//! ```text
//! claude / opencode / codex / gemini / any binary
//!         │  (real upstream process, unmodified, in a real PTY)
//!         ▼
//! rime-agentd  — unprivileged, per-user, systemd --user
//!         ├─ PTY + session lifecycle       session.rs
//!         ├─ the six permission dimensions policy.rs
//!         ├─ system-access grants          grant.rs
//!         ├─ proving a human is present    auth.rs
//!         ├─ where a request came from     origin.rs
//!         ├─ what a screen lock means      lock.rs
//!         ├─ a security key was touched    webauthn.rs
//!         ├─ sandbox policy                sandbox.rs
//!         ├─ network destination policy    destination.rs
//!         ├─ adapters                      adapter.rs
//!         ├─ agent profiles                profile.rs
//!         ├─ claude's own hook lifecycle   hook.rs
//!         ├─ handing a file to a session   inject.rs
//!         ├─ projects + worktrees          project.rs
//!         ├─ per-worktree status            worktree.rs
//!         ├─ project window layouts        layout.rs
//!         ├─ checkpoints                   checkpoint.rs
//!         ├─ privilege requests            request.rs
//!         └─ the audit half nobody can edit journal.rs
//!         ▲
//!         │  newline-delimited JSON on a Unix socket    protocol.rs
//! rime agent … / Rime Shell
//! ```
//!
//! Nothing here is privileged, and nothing here talks to `rimed`. Agent
//! orchestration in the privileged daemon is exactly what the roadmap forbids.
//! A session that needs a system change files a structured request
//! (`request.rs`) which a human approves, and the operation then runs with that
//! human's own privilege — this process never gains rights.

pub mod adapter;
pub mod auth;
pub mod checkpoint;
pub mod client;
pub mod config;
pub mod destination;
pub mod filename;
pub mod git;
pub mod grant;
pub mod graph;
pub mod handoff;
pub mod hook;
pub mod inject;
pub mod journal;
pub mod layout;
pub mod mcpconf;
pub mod mux;
pub mod lock;
pub mod origin;
pub mod paths;
pub mod pluginconf;
pub mod policy;
pub mod profile;
pub mod project;
pub mod protocol;
pub mod request;
pub mod sandbox;
pub mod session;
pub mod statusline;
pub mod term;
pub mod webauthn;
pub mod worktree;

pub use policy::AgentPolicy;
pub use protocol::{AgentState, SandboxPolicy, SessionInfo};
