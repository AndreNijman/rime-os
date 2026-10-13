//! The verbs, and who may ask for them.
//!
//! One method per request. Every one of them starts from the *peer's* uid,
//! never from anything the request carried, because the request is written by
//! whoever connected.
//!
//! ## The two authorisation questions
//!
//! **Whose credentials are these?** The uid from `SO_PEERCRED`. The store is
//! per-uid, so a request only ever reaches the connecting account's own
//! namespace and there is no verb that names another.
//!
//! **May this caller change what is allowed?** Only if it is not inside a
//! managed agent session — see `peer::looks_like_a_session`. A session that can
//! grant itself a capability has no capabilities, so `add`, `remove`, `grant`
//! and `revoke` are refused for one. `use` is not: performing a granted
//! operation is the entire point.
//!
//! That second check is a *hardening* measure, not the confidentiality
//! boundary. It rests on `/proc` and cgroup membership, both of which a
//! determined same-uid process can eventually work around. What it cannot work
//! around is the store being root-owned and the protocol having no verb that
//! returns a value.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use apex_secret_core::audit::{self, AuditEvent, AuditLine};
use apex_secret_core::budget::{self, Budget, Usage};
use apex_secret_core::capability::{self, CapabilityRecord, EndpointError};
use apex_secret_core::operation::OperationSpec;
use apex_secret_core::protocol::{ErrorKind, Response};
use apex_secret_core::store::{self, ServiceInfo, Store, StoreError};
use apex_secret_core::SecretValue;

use crate::broker;
use crate::peer::Peer;
use crate::provider::{self, Registry};

/// How far in the future a record's expiry may sit.
///
/// A capability request is not a bearer token here — the daemon performs the
/// operation before it replies — so this bounds a queued or replayed request
/// rather than a credential in somebody's hands.
const MAX_EXPIRY_AHEAD_MS: u64 = 60 * 60 * 1000;

/// §11's `approval_policy` on a request that needed the owner and did not have
/// them.
///
/// Its own word, distinct from `grant`, because the refused line would
/// otherwise say the request was authorised by a grant — which is true and
/// misleading at once: the grant was there, and it was not enough.
pub const APPROVAL_REQUIRED: &str = "approval-required";

/// §11's `approval_policy` on a request the owner approved, once.
pub const OWNER: &str = "owner";

/// How many extra attempts an operation gets when the far side refuses the
/// **minted** credential it is running on.
///
/// Measured, not chosen: against `api.cloudflare.com` on 2026-09-12 a token
/// seconds old was refused by D1's API on every attempt at no delay and
/// accepted after a three-second pause on every attempt at three, five and
/// twenty. Two retries at [`NARROWED_REFUSAL_PAUSE`] and twice that give a
/// token nine seconds to become real — three times the window that was
/// actually needed — and cost nothing at all on the ordinary path, because an
/// operation whose credential was accepted never enters the loop.
pub const NARROWED_REFUSAL_RETRIES: u32 = 2;

/// How long to wait before trying a refused minted credential again. The
/// second retry waits twice this.
pub const NARROWED_REFUSAL_PAUSE: std::time::Duration = std::time::Duration::from_secs(3);

/// The daemon's state.
pub struct Service {
    store: Store,
    /// Whether the store is genuinely out of the user's reach — true when the
    /// daemon runs as root. Reported in `hello` rather than assumed, so a
    /// daemon started by hand for a test does not imply a protection it does
    /// not have.
    protected: bool,
    /// The providers this daemon serves. Fixed at startup: a request cannot
    /// cause a provider to appear, and the vocabulary it advertises is the one
    /// it can actually perform.
    registry: Registry,
    audit_counter: AtomicU64,
    /// How long to wait before trying a refused minted credential again.
    ///
    /// A field rather than [`NARROWED_REFUSAL_PAUSE`] read directly, for the
    /// same reason the device flow's `floor` is one: a test has to exercise
    /// the branch without spending nine real seconds in a sleep, and a test
    /// that skipped the sleep by skipping the code would be measuring
    /// something else.
    refusal_pause: std::time::Duration,
    /// Held across the read-check-write of one account's approvals file.
    ///
    /// `main.rs` serves one thread per connection, so every store file this
    /// daemon writes is read-modify-written concurrently. For grants that is
    /// benign — `allow` and `revoke` are idempotent, and two callers making the
    /// same grant twice make it once. For an approval it is not: an approval is
    /// **spent**, and two operations that read the file at the same moment
    /// would both find it, both perform, and both write back a file missing one
    /// entry. That is a single-use approval used twice, which is the one thing
    /// this whole mechanism exists to prevent.
    ///
    /// One mutex for every account rather than one per uid: the critical
    /// section is two small file operations, contention between two accounts
    /// deploying at the same instant costs microseconds, and a map of mutexes
    /// keyed on a number the caller influences is a way to grow memory.
    ///
    /// This is enough **because this daemon is the only writer**. The store is
    /// root-owned and `0700`, and `apex secret approve` reaches it through this
    /// socket like everything else; there is no second process to race with.
    approvals: Mutex<()>,
    /// §13.14: budgeted operations that have passed the check and have not yet
    /// reached the trail.
    ///
    /// A budget is counted from the audit trail, and a line only appears there
    /// when the operation is over. Between the check and the line the operation
    /// is invisible — so with one thread per connection, a cap of five admits
    /// however many callers happen to be inside that window at once. That is a
    /// cap that does nothing under exactly the load a cap is for.
    ///
    /// So a passing check leaves a reservation here, the next check counts the
    /// trail *plus* the reservations, and the reservation is dropped when the
    /// request leaves `use_capability` — by which point either a line has been
    /// written or the request was refused and never happened. A reservation
    /// that outlives its usefulness makes the budget stricter for a moment and
    /// never looser, which is the direction to be wrong in.
    ///
    /// Keyed by account and project root, holding one entry per in-flight
    /// `(credential, operation)`. The lock is taken for the check and for the
    /// release, never across `perform` — holding it there would serialise every
    /// budgeted deployment on the machine behind the slowest one.
    reservations: Mutex<Reservations>,
}

/// In-flight budgeted operations, by account and project root, each entry a
/// `(credential, operation)` pair that has passed the check and not yet reached
/// the trail.
type Reservations = BTreeMap<(u32, String), Vec<(String, String)>>;

/// What §13.14's check answers with when an operation may proceed.
struct Budgeted<'a> {
    /// The word for [`AuditLine::spend`] — `within`, or `no-budget`.
    word: String,
    /// The detail behind it, when there is one.
    detail: Option<String>,
    /// Held until the operation reaches the trail; `None` for a project with no
    /// budget, which has no headroom to reserve.
    reserved: Option<Reservation<'a>>,
}

/// One in-flight budgeted operation, released when it goes out of scope.
///
/// A guard rather than a matching `release` call at each exit, because
/// [`Service::use_capability`] returns from a dozen places after the check and
/// a reservation leaked by one of them would cap that project at its current
/// usage until the daemon restarted.
pub struct Reservation<'a> {
    service: &'a Service,
    key: (u32, String),
    entry: (String, String),
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let Ok(mut held) = self.service.reservations.lock() else {
            return;
        };
        if let Some(list) = held.get_mut(&self.key) {
            if let Some(at) = list.iter().position(|e| e == &self.entry) {
                list.swap_remove(at);
            }
            if list.is_empty() {
                held.remove(&self.key);
            }
        }
    }
}

impl Service {
    pub fn new(store: Store, protected: bool, registry: Registry) -> Service {
        Service {
            store,
            protected,
            registry,
            audit_counter: AtomicU64::new(0),
            refusal_pause: NARROWED_REFUSAL_PAUSE,
            approvals: Mutex::new(()),
            reservations: Mutex::new(BTreeMap::new()),
        }
    }

    /// Shorten the wait between retries of a refused minted credential.
    ///
    /// Tests only, and it takes a duration rather than switching the sleep off
    /// so that the loop a test measures is the loop that ships.
    #[cfg(test)]
    pub fn with_refusal_pause(mut self, pause: std::time::Duration) -> Service {
        self.refusal_pause = pause;
        self
    }

    fn next_audit_id(&self) -> String {
        audit::mint_id(self.audit_counter.fetch_add(1, Ordering::Relaxed))
    }

    fn record(&self, line: AuditLine) {
        if let Err(e) = audit::append(&self.store.audit_path(), &line) {
            eprintln!("apex-secretd: cannot write the audit trail: {e}");
        }
    }

    // ── metadata ────────────────────────────────────────────────────────────

    pub fn hello(&self) -> Response {
        Response::Hello {
            version: apex_secret_core::protocol::PROTOCOL_VERSION,
            capabilities: self.registry.operation_ids(),
            vocabulary: self.registry.vocabulary(),
            protected: self.protected,
        }
    }

    pub fn list(&self, peer: Peer) -> Response {
        Response::Services {
            services: self.store.list(peer.uid),
        }
    }

    pub fn grants(&self, peer: Peer) -> Response {
        Response::Grants {
            projects: self.store.grants(peer.uid).projects,
        }
    }

    /// The audit trail, filtered to the caller's own account.
    ///
    /// One file for the machine, because "what happened at 14:02" should not be
    /// a join across accounts — but a caller sees only its own lines, so the
    /// trail does not become a way to learn which services another account has.
    pub fn audit(&self, peer: Peer, lines: usize, project: Option<&str>) -> Response {
        let limit = lines.clamp(1, 1000);
        // The project filter runs BEFORE the limit is applied, which is the
        // whole reason it exists on the wire rather than in the caller: the
        // trail is one file for the machine, so a client filtering the last
        // thousand lines itself would see a thousand lines from everywhere
        // else and conclude this project had done nothing.
        let mut entries: Vec<AuditLine> = audit::tail(&self.store.audit_path(), 10_000)
            .into_iter()
            .filter(|l| l.uid == peer.uid)
            .filter(|l| match project {
                // An exact match on the root, never a prefix. A worktree lives
                // at `<project>/.apex/worktrees/<name>`, so a prefix match
                // asking about the project would sweep in every worktree's
                // work — and a destroy plan built from that would offer to
                // delete another worktree's preview.
                Some(want) => l.project.as_deref() == Some(want),
                None => true,
            })
            .collect();
        if entries.len() > limit {
            entries.drain(..entries.len() - limit);
        }
        Response::Audit { entries }
    }

    /// §13.14: what this project's budget allows and what it has spent today.
    ///
    /// The same reader, the same counter and the same trail the enforcement
    /// uses — so the report cannot say a project is inside its budget while the
    /// broker refuses it, which is what a second implementation in the CLI
    /// would eventually do.
    ///
    /// A budget that cannot be read is an error here, exactly as it is a
    /// refusal there. A report that quietly showed "no budget" for a project
    /// whose `apex.toml` is unreadable would be the friendliest possible way to
    /// tell somebody their cap is fine while it is not being enforced.
    pub fn usage(&self, peer: Peer, project: &str) -> Response {
        if !broker::valid_project(project) {
            return Response::error(
                ErrorKind::BadRequest,
                format!("'{}' is not a project root", project.escape_debug()),
            );
        }
        let Some(owner) = broker::owner(peer.uid) else {
            return Response::error(
                ErrorKind::PermissionDenied,
                format!("uid {} is not an account on this machine", peer.uid),
            );
        };
        let budget = match Budget::read_or_unbudgeted(Path::new(project), owner.uid, &owner.name) {
            Ok(budget) => budget,
            Err(e) => {
                return Response::error(
                    ErrorKind::BadRequest,
                    format!(
                        "this project's budget could not be read, so what it \
                         allows is not known: {e}"
                    ),
                )
            }
        };
        let usage = Usage::of(
            &audit::tail(&self.store.audit_path(), usize::MAX),
            peer.uid,
            project,
            store::now_ms(),
        );
        Response::Usage {
            report: Box::new(budget::Report::new(project, &budget, &usage)),
        }
    }

    // ── administration, owner only ──────────────────────────────────────────

    pub fn add(&self, peer: Peer, new: NewService<'_>, value: SecretValue) -> Response {
        let NewService {
            service,
            host,
            scheme,
            username,
            path,
            auth,
            port,
        } = new;
        if !store::valid_service_name(service) {
            return refuse_store(StoreError::BadServiceName(service.to_string()));
        }
        if host.is_empty() || host.len() > 253 || !host.chars().all(valid_host_char) {
            return Response::error(
                ErrorKind::BadRequest,
                format!("'{}' is not a host name", host.escape_debug()),
            );
        }
        if scheme != "https" && scheme != "http" {
            return Response::error(
                ErrorKind::BadRequest,
                format!(
                    "'{}' is not a scheme this service brokers; use https, or \
                     http for a loopback host",
                    scheme.escape_debug()
                ),
            );
        }
        if !store::valid_endpoint_path(path) {
            return Response::error(
                ErrorKind::BadRequest,
                format!(
                    "'{}' is not an endpoint path; it must start with '/' and \
                     carry no query, fragment or '..'",
                    path.escape_debug()
                ),
            );
        }
        let auth = auth.unwrap_or("bearer");
        if auth != "bearer" && auth != "raw" {
            return Response::error(
                ErrorKind::BadRequest,
                format!(
                    "'{}' is not an auth scheme; use bearer or raw",
                    auth.escape_debug()
                ),
            );
        }
        let info = ServiceInfo {
            service: service.to_string(),
            host: host.to_ascii_lowercase(),
            scheme: scheme.to_string(),
            username: username.unwrap_or("x-access-token").to_string(),
            path: path.to_string(),
            auth: auth.to_string(),
            port,
            added: store::now_ms(),
        };
        if let Err(e) = self.store.put(peer.uid, &info, &value) {
            return refuse_store(e);
        }
        self.record(AuditLine::administrative(
            &self.next_audit_id(),
            AuditEvent::Stored,
            peer.uid,
            peer.pid,
            service,
            &format!("credential for {}://{}", info.scheme, info.host),
        ));
        Response::Ok
    }

    pub fn remove(&self, peer: Peer, service: &str) -> Response {
        match self.store.remove(peer.uid, service) {
            Ok(()) => {
                self.record(AuditLine::administrative(
                    &self.next_audit_id(),
                    AuditEvent::Removed,
                    peer.uid,
                    peer.pid,
                    service,
                    "credential, every grant that named it, and every approval",
                ));
                Response::Ok
            }
            Err(e) => refuse_store(e),
        }
    }

    pub fn grant(
        &self,
        peer: Peer,
        project: &str,
        service: &str,
        capability: &str,
        revoke: bool,
    ) -> Response {
        let everywhere = project == store::ANY_PROJECT;
        if !everywhere && !broker::valid_project(project) {
            return Response::error(
                ErrorKind::BadRequest,
                "a grant is keyed on an absolute project path".to_string(),
            );
        }
        // Through the registry, so a grant is written under the operation's
        // canonical id whichever spelling was typed — and an operation no
        // provider offers cannot be granted at all.
        let op = match self.registry.lookup(capability) {
            Ok((_, op)) => op,
            Err(e) => return Response::error(ErrorKind::BadRequest, e.to_string()),
        };
        // The gate on `*`, here rather than in the CLI, because the CLI is not
        // the trust boundary: this socket takes a request from anything running
        // as the account. See [`may_be_granted_everywhere`] for what earns it.
        if everywhere && !may_be_granted_everywhere(op) {
            return Response::error(
                ErrorKind::BadRequest,
                format!(
                    "'{}' resolves against the project it is asked in, so it is granted \
                     where you name it and not everywhere. Run `apex secret grant` in \
                     the project.",
                    op.id
                ),
            );
        }
        let capability = op.id;
        if !store::valid_service_name(service) {
            return refuse_store(StoreError::BadServiceName(service.to_string()));
        }

        let mut grants = self.store.grants(peer.uid);
        if revoke {
            // Every name the operation answers to, not just the canonical one.
            // A grant written before a rename is on disk under the old
            // spelling, and a revoke that missed it would report "was not
            // granted" and leave it there.
            if !grants.revoke_any(project, service, &provider::grant_names(op)) {
                return Response::error(
                    ErrorKind::NoSuchService,
                    format!("{service}:{capability} was not granted for {project}"),
                );
            }
        } else {
            // A capability cannot be granted for a credential that does not
            // exist: otherwise a typo produces a grant that silently never
            // matches.
            if self.store.info(peer.uid, service).is_none() {
                return refuse_store(StoreError::NoSuchService(service.to_string()));
            }
            grants.allow(project, service, capability);
        }
        if let Err(e) = self.store.save_grants(peer.uid, &grants) {
            return refuse_store(e);
        }
        self.record(AuditLine::administrative(
            &self.next_audit_id(),
            if revoke {
                AuditEvent::Revoked
            } else {
                AuditEvent::Granted
            },
            peer.uid,
            peer.pid,
            service,
            &format!("{capability} for {project}"),
        ));
        Response::Grants {
            projects: grants.projects,
        }
    }

    /// §13.8: approve one operation, once, or take an approval back.
    ///
    /// ## What this is not
    ///
    /// It is not a grant, and the differences are the design rather than
    /// details. A grant is consulted and stays; this is spent by the first
    /// matching operation and expires on its own. A grant is keyed on the
    /// operation; this is keyed on the operation **and the resource**, because
    /// approving the preview deploy must not approve the production one. And
    /// [`store::ANY_PROJECT`] is refused outright — an approval that applied
    /// wherever the agent happened to be standing would be standing permission
    /// with a shorter life, which is precisely the thing §13.8 says has to be
    /// written down explicitly instead.
    ///
    /// ## Why the resource is not checked for existence
    ///
    /// The owner may approve a deployment of a worker that has never been
    /// deployed, in a project whose `apex.toml` they are about to write. What
    /// resolves a name is a provider's `bind`, which needs a credential in
    /// scope and a request in flight; running it here would mean resolving the
    /// caller's project twice for a verb that authorises nothing by itself. An
    /// approval that matches nothing is harmless: it expires.
    pub fn approve(&self, peer: Peer, asked: NewApproval<'_>) -> Response {
        let NewApproval {
            project,
            service,
            operation,
            resource,
            ttl_ms,
            withdraw,
        } = asked;
        if !broker::valid_project(project) {
            return Response::error(
                ErrorKind::BadRequest,
                "an approval is for one operation in one project, named by its \
                 absolute path"
                    .to_string(),
            );
        }
        // Through the registry, so an approval is written under the canonical
        // id whichever spelling was typed, and an operation nothing offers
        // cannot be approved at all.
        let op = match self.registry.lookup(operation) {
            Ok((_, op)) => op,
            Err(e) => return Response::error(ErrorKind::BadRequest, e.to_string()),
        };
        // The resource the approval names is the one a request will carry, so
        // it is checked with the operation's own grammar. Without this an
        // owner could approve `../../etc` and be told it had worked, and then
        // wonder why the deployment they approved was still refused.
        //
        // The RESOURCE only. An approval names no options on purpose — see
        // [`store::Approval`] — so checking the full declaration here would
        // refuse every approval for an operation with a required parameter,
        // which is most of the ones worth approving.
        if let Err(e) = op.check_resource(resource) {
            return Response::error(ErrorKind::BadRequest, e.to_string());
        }
        if !store::valid_service_name(service) {
            return refuse_store(StoreError::BadServiceName(service.to_string()));
        }
        let ttl = ttl_ms.unwrap_or(store::APPROVAL_TTL_MS);
        // Refused rather than clamped, for `deploy::share`'s reason: somebody
        // who wrote a week meant a week, and silently giving them a day would
        // be this service deciding what they meant.
        if !withdraw && (ttl == 0 || ttl > store::MAX_APPROVAL_TTL_MS) {
            return Response::error(
                ErrorKind::BadRequest,
                format!(
                    "an approval lives between 1 and {} milliseconds, and \
                     {ttl} is not in that range",
                    store::MAX_APPROVAL_TTL_MS
                ),
            );
        }

        let now = store::now_ms();
        let (pending, event, detail) = {
            let _held = self.approvals.lock().expect("approvals lock");
            let mut approvals = self.store.approvals(peer.uid);
            approvals.prune(now);
            if withdraw {
                if !approvals.withdraw(project, service, &provider::grant_names(op), resource) {
                    return Response::error(
                        ErrorKind::NoSuchService,
                        format!(
                            "nothing is approved for {}{} in {project}",
                            op.id,
                            if resource.is_empty() {
                                String::new()
                            } else {
                                format!(" {resource}")
                            }
                        ),
                    );
                }
            } else {
                // A credential that does not exist cannot be approved against,
                // for `grant`'s reason: otherwise a typo produces an approval
                // that silently never matches, and the owner believes they
                // approved something.
                if self.store.info(peer.uid, service).is_none() {
                    return refuse_store(StoreError::NoSuchService(service.to_string()));
                }
                approvals.approve(store::Approval {
                    project: project.to_string(),
                    service: service.to_string(),
                    operation: op.id.to_string(),
                    resource: resource.to_string(),
                    granted_ms: now,
                    expires_ms: now.saturating_add(ttl),
                });
            }
            if let Err(e) = self.store.save_approvals(peer.uid, &approvals) {
                return refuse_store(e);
            }
            (
                approvals.pending.clone(),
                if withdraw {
                    AuditEvent::Withdrawn
                } else {
                    AuditEvent::Approved
                },
                format!("{} {resource} in {project}", op.id),
            )
        };
        self.record(AuditLine::administrative(
            &self.next_audit_id(),
            event,
            peer.uid,
            peer.pid,
            service,
            detail.trim_end(),
        ));
        Response::Approvals { pending }
    }

    /// What the owner has outstanding, soonest to expire first.
    ///
    /// Expired ones are dropped on the way past rather than shown: an approval
    /// that cannot be spent is not outstanding, and listing it would invite
    /// somebody to believe the operation it names is still authorised.
    pub fn approvals(&self, peer: Peer) -> Response {
        let now = store::now_ms();
        let _held = self.approvals.lock().expect("approvals lock");
        let mut approvals = self.store.approvals(peer.uid);
        if approvals.prune(now) > 0 {
            // Best effort: a list that could not tidy up is still an honest
            // list, because `prune` already removed them from what is returned.
            let _ = self.store.save_approvals(peer.uid, &approvals);
        }
        approvals.pending.sort_by_key(|a| a.expires_ms);
        Response::Approvals {
            pending: approvals.pending,
        }
    }

    /// Spend the approval this request needs, or say why there is none.
    ///
    /// Held under [`Service::approvals`]' mutex for the whole read-check-write,
    /// so two requests cannot both find the same approval. The `Ok` arm means
    /// one was found **and the file recording that it is gone has been
    /// written** — a spend that was not persisted is a spend that did not
    /// happen, and returning success for one would be a single-use approval
    /// that survives its use.
    fn spend_approval(
        &self,
        peer: Peer,
        project: &str,
        service: &str,
        op: &'static OperationSpec,
        resource: &str,
        now: u64,
    ) -> Result<(), String> {
        let _held = self.approvals.lock().expect("approvals lock");
        let mut approvals = self.store.approvals(peer.uid);
        approvals.prune(now);
        let names = provider::grant_names(op);
        if approvals
            .spend(project, service, &names, resource, now)
            .is_none()
        {
            return Err(String::new());
        }
        self.store
            .save_approvals(peer.uid, &approvals)
            .map_err(|e| {
                format!(
                    "the owner's approval for this operation could not be \
                     spent: {e}. It has NOT been used and the operation has \
                     not run"
                )
            })
    }

    /// §13.14: whether one more operation fits inside the project's budget.
    ///
    /// Returns the verdict, the words to put on the audit line, and — when the
    /// answer is a pass and there was a budget at all — the reservation that
    /// keeps a concurrent check from spending the same headroom twice.
    ///
    /// A project with no budget pays nothing: the file is read, found to
    /// declare none, and the trail is never touched. That matters because this
    /// runs on every brokered operation on the machine, and the trail is one
    /// file that is read whole.
    fn check_budget(
        &self,
        peer: Peer,
        project: &str,
        owner: &broker::Owner,
        service: &str,
        op: &'static OperationSpec,
        now: u64,
    ) -> Result<Budgeted<'_>, (String, String)> {
        let budget = match Budget::read_or_unbudgeted(Path::new(project), owner.uid, &owner.name) {
            Ok(budget) => budget,
            // Not folded into "no budget". A budget file that could not be read
            // is not a project without a budget, and an agent that can make
            // `apex.toml` unreadable would otherwise be an agent that can
            // remove its own cap.
            Err(e) => {
                return Err((
                    budget::UNMEASURABLE.to_string(),
                    format!(
                        "this project's budget could not be read, so nothing \
                         here knows whether this operation fits inside it, and \
                         it has not run: {e}"
                    ),
                ))
            }
        };
        if budget.is_empty() {
            return Ok(Budgeted {
                word: audit::NO_BUDGET.to_string(),
                detail: None,
                reserved: None,
            });
        }
        // A cap keyed by an operation id nothing implements never bites. The
        // vocabulary is right here, so it is checked here rather than being
        // discovered months later by somebody wondering why the cap did
        // nothing.
        let unknown = budget.unknown_operations(&self.registry.operation_ids());
        if !unknown.is_empty() {
            // An alias is a near miss worth naming separately. A grant may be
            // written `memory:mcp-request`, so somebody capping the same thing
            // reaches for the same spelling — but the trail records the
            // canonical id, so an alias cap would match nothing. Saying which
            // to write is the difference between a refusal and a puzzle.
            let named: Vec<String> = unknown
                .iter()
                .map(|name| match self.registry.lookup(name) {
                    Ok((_, op)) => format!("'{name}' (an alias; write '{}')", op.id),
                    Err(_) => format!("'{name}'"),
                })
                .collect();
            return Err((
                budget::UNMEASURABLE.to_string(),
                format!(
                    "this project's [agent.budget] caps or prices {}, which is \
                     not how this machine spells an operation it can count — so \
                     the cap would never bite, and a cap that does nothing is \
                     worse than no cap. `apex secret capabilities` lists what \
                     can be capped",
                    named.join(", ")
                ),
            ));
        }

        let mut held = self.reservations.lock().map_err(|_| {
            (
                budget::UNMEASURABLE.to_string(),
                "this daemon's budget bookkeeping is poisoned, so whether this \
                 operation fits inside the project's budget is not known and \
                 it has not run"
                    .to_string(),
            )
        })?;
        let key = (peer.uid, project.to_string());
        // The whole trail, not a window: `audit::tail`'s limit exists for a
        // reader with a screen, and a budget counted from the last N lines is a
        // budget a busy machine can hide an operation from.
        let mut usage = Usage::of(
            &audit::tail(&self.store.audit_path(), usize::MAX),
            peer.uid,
            project,
            now,
        );
        for (ran_under, operation) in held.get(&key).into_iter().flatten() {
            usage.add(ran_under, operation);
        }

        let spend = budget::check(&budget, &usage, service, op.id);
        let word = spend.as_str().to_string();
        let why = spend.reason().map(str::to_string);
        if !matches!(spend, budget::Spend::Within) {
            return Err((word, why.unwrap_or_default()));
        }
        let entry = (service.to_string(), op.id.to_string());
        held.entry(key.clone()).or_default().push(entry.clone());
        Ok(Budgeted {
            word,
            detail: why,
            reserved: Some(Reservation {
                service: self,
                key,
                entry,
            }),
        })
    }

    // ── the framework ───────────────────────────────────────────────────────

    /// Why a request was allowed, in §11's `approval_policy` vocabulary.
    ///
    /// Its own step, and its own type, because it is the seam the permission
    /// work attaches to: break-glass and session-scoped grants (P0-005) are
    /// more answers here, not more branches in the middle of the pipeline.
    ///
    /// The value is **decided by the daemon**. A record arrives carrying an
    /// `approval_policy` because §11 puts one in the record, and it is
    /// overwritten — the same treatment `resource` gets, and for the same
    /// reason: a caller that could name how it was approved could write
    /// `owner` into a trail nobody read.
    fn decide(
        &self,
        peer: Peer,
        project: &str,
        provider: &str,
        op: &'static OperationSpec,
    ) -> Decision {
        let names = provider::grant_names(op);
        if self
            .store
            .grants(peer.uid)
            .allows_any(Some(project), provider, &names)
        {
            return Decision::Allowed("grant");
        }
        // The one message somebody reads in a worktree where a global MCP
        // server has stopped working, relayed to them by the agent as a
        // JSON-RPC error. It has to carry the way out, and for an operation
        // that names nothing the way out is usually `--everywhere` rather than
        // running the same grant again in each new directory.
        let everywhere = if may_be_granted_everywhere(op) {
            format!(
                ", or in every project with `apex secret grant {provider} {} --everywhere`",
                op.id
            )
        } else {
            String::new()
        };
        Decision::Refused(format!(
            "'{}' on '{provider}' is not granted for this project; allow it with \
             `apex secret grant {provider} {}`{everywhere}",
            op.id, op.id
        ))
    }

    /// Perform a capability.
    ///
    /// The order is the security argument, and every step before the last can
    /// refuse. Every step except 9 and 12 belongs to the framework and applies
    /// to every provider that will ever be registered; those two are the
    /// provider's, and neither of them decides whether the request was
    /// allowed.
    ///
    ///  1. the caller's account, from the kernel — never from the request;
    ///  2. the operation exists, in a registered provider's vocabulary;
    ///  3. its arguments are the ones that provider declared;
    ///  4. the record is well formed and has not expired;
    ///  5. a credential exists under that name, for that account;
    ///  6. the capability is granted for that project — [`Service::decide`];
    ///  7. §13.14: one more operation fits inside the project's budget, and a
    ///     reservation is taken so a concurrent request cannot spend the same
    ///     headroom — [`Service::check_budget`];
    ///  8. only then may the provider touch the caller's machine;
    ///  9. the provider resolves what the caller named, and says where the
    ///     credential would go;
    /// 10. that endpoint is pinned against the one the credential was stored
    ///     for — the provider does not get to skip this, because it has not
    ///     been given the value yet;
    /// 11. §13.8: where the provider said the grant is not enough, the owner's
    ///     one-shot approval is found and SPENT, or the request is refused;
    /// 12. the value is read, once, and the provider presents it;
    /// 13. the value — and any short-lived one minted from it — is scrubbed
    ///     out of everything returned;
    /// 14. the trail records the record the decision was made on.
    ///
    /// Step 7 sits where it does on purpose: before the provider runs anything
    /// on the caller's machine, and before the owner's one-shot approval can be
    /// spent. An over-budget request that burned an approval would be an
    /// approval given for a deployment and consumed by an attempt.
    ///
    /// `body` is the message an operation carries, for the one operation that
    /// carries one. It is not checked here and never can be: what a message
    /// means is the provider's, and the framework's job is to make sure the
    /// operation carrying it was allowed. Empty for every git operation.
    pub fn use_capability(
        &self,
        peer: Peer,
        mut record: CapabilityRecord,
        body: Vec<u8>,
    ) -> Response {
        let audit_id = self.next_audit_id();
        record.audit_id = audit_id.clone();
        // §11's `approval_policy` is the daemon's answer, and until `decide`
        // gives one there is no answer. Cleared before the refusal path can
        // capture it: a caller that arrived claiming `owner` must not have that
        // word appear on a `refused` line, where nobody would think to doubt it.
        record.approval_policy = UNDECIDED.to_string();

        // §13.14's verdict, as it stands at each point a refusal can happen.
        // It starts as "nobody has looked", becomes the budget's answer once
        // the check has run, and every audit line written from here reads it —
        // so a request refused for a host mismatch *after* the budget passed
        // says the budget passed, rather than claiming the check never ran.
        // A cell because `refuse` borrows it and the check writes it.
        let spend = RefCell::new((audit::NOT_CHECKED.to_string(), None::<String>));

        let refuse = |record: &CapabilityRecord, reason: String, kind: ErrorKind| -> Response {
            let (word, detail) = spend.borrow().clone();
            self.record(AuditLine {
                reason: Some(reason.clone()),
                spend: word,
                spend_detail: detail,
                ..AuditLine::from_record(&audit_id, AuditEvent::Refused, peer.uid, peer.pid, record)
            });
            Response::error(kind, reason)
        };

        if !store::valid_service_name(&record.provider) {
            return refuse(
                &record,
                StoreError::BadServiceName(record.provider.clone()).to_string(),
                ErrorKind::BadRequest,
            );
        }
        // The two §7 fields are claims forwarded by `apex-agentd`, and this
        // daemon cannot re-derive either: the connection it sees is the agent
        // runtime's, not the session's. So it checks the shape — the trail is
        // line-delimited JSON somebody greps, and a caller that could write a
        // newline or a paragraph into it could shape what the audit looks like
        // — and records the claim as a claim.
        for (field, value) in [
            ("request_origin", &record.request_origin),
            ("origin_source", &record.origin_source),
        ] {
            if !capability::valid_origin_label(value) {
                return refuse(
                    &record,
                    format!("'{}' is not a {field} label", value.escape_debug()),
                    ErrorKind::BadRequest,
                );
            }
        }

        // The vocabulary, closed by what is registered. An operation no
        // provider declares never reaches a credential — the same property
        // P0-002's closed enum had, held by the registry instead so that adding
        // a provider does not mean editing a type in `apex-secret-core`.
        let (backend, op) = match self.registry.lookup(&record.operation) {
            Ok(found) => found,
            Err(e) => return refuse(&record, e.to_string(), ErrorKind::BadRequest),
        };
        // The canonical id replaces whatever spelling arrived, so the trail and
        // the grant table cannot end up describing the same operation two ways.
        record.operation = op.id.to_string();
        if let Err(e) = op.check(&record.resource, &record.params) {
            return refuse(&record, e.to_string(), ErrorKind::BadRequest);
        }

        let now = store::now_ms();
        if record.is_expired(now) {
            return refuse(
                &record,
                "that capability request has expired; ask for it again".to_string(),
                ErrorKind::PermissionDenied,
            );
        }
        if record.expiry.is_some_and(|at| at > now + MAX_EXPIRY_AHEAD_MS) {
            return refuse(
                &record,
                "that capability request is valid for longer than this service \
                 will honour"
                    .to_string(),
                ErrorKind::BadRequest,
            );
        }

        let Some(project) = record.project.clone().filter(|p| broker::valid_project(p)) else {
            return refuse(
                &record,
                "this request names no project, so no grant can match it".to_string(),
                ErrorKind::PermissionDenied,
            );
        };

        let Some(owner) = broker::owner(peer.uid) else {
            return refuse(
                &record,
                format!("uid {} is not an account on this machine", peer.uid),
                ErrorKind::PermissionDenied,
            );
        };

        let Some(info) = self.store.info(peer.uid, &record.provider) else {
            return refuse(
                &record,
                StoreError::NoSuchService(record.provider.clone()).to_string(),
                ErrorKind::NoSuchService,
            );
        };

        // Decided here, and stamped on the record before anything is performed,
        // so the trail says how this was authorised even if the operation then
        // fails.
        match self.decide(peer, &project, &record.provider, op) {
            Decision::Allowed(policy) => record.approval_policy = policy.to_string(),
            Decision::Refused(reason) => {
                return refuse(&record, reason, ErrorKind::PermissionDenied)
            }
        }

        // §13.14, and the cheapest thing that can refuse.
        //
        // BEFORE `bind`, for the reason every framework check is: the provider
        // does not touch the caller's machine for a request that is not going
        // to happen. And before the §13.8 approval, so an over-budget request
        // cannot spend the owner's one-shot approval on an operation that was
        // never going to run — they approved a deployment, not an attempt.
        //
        // The reservation is held until this function returns, which is after
        // the trail line is written. See `Service::reservations`.
        let _reserved = match self.check_budget(peer, &project, &owner, &record.provider, op, now) {
            Ok(budgeted) => {
                *spend.borrow_mut() = (budgeted.word, budgeted.detail);
                budgeted.reserved
            }
            Err((word, reason)) => {
                // The verdict reaches the line through the cell `refuse`
                // reads, because the line is the whole of "usage visible in
                // the task audit" for a request that did NOT run.
                *spend.borrow_mut() = (word, Some(reason.clone()));
                return refuse(&record, reason, ErrorKind::PermissionDenied);
            }
        };

        // Everything the framework can check has passed, so the provider may
        // now touch the caller's machine. For git that is a `git remote
        // get-url` in a caller-controlled repository, as the owner; doing it
        // before the grant check would run it for a request that was never
        // allowed.
        let req = provider::Bind {
            operation: op,
            resource: &record.resource,
            params: &record.params,
            body: &body,
            project: &project,
            service: &info,
            owner: &owner,
            audit_id: &audit_id,
        };
        let bound = match backend.bind(&req) {
            Ok(bound) => bound,
            Err(e) => return refuse(&record, e.to_string(), kind_of(&e)),
        };

        // The pin, in the framework and not in the provider. A credential
        // stored for one host may only ever be sent to that host, whatever the
        // provider resolved and whoever wrote the provider.
        if bound.endpoint.scheme != info.scheme
            || bound.endpoint.host != info.host.to_ascii_lowercase()
        {
            return refuse(
                &record,
                EndpointError::HostMismatch {
                    remote_host: bound.endpoint.to_string(),
                    service_host: format!("{}://{}", info.scheme, info.host),
                }
                .to_string(),
                ErrorKind::PermissionDenied,
            );
        }
        let endpoint = bound.endpoint.to_string();

        // §13.10's half that has to happen BEFORE anything runs.
        //
        // An operation that will create a credential — an Access service token,
        // a Tunnel credential — declares the name it would be stored under when
        // it binds. The far side issues such a secret **once** and never shows
        // it again, so a name that is already taken has to be refused here,
        // while refusing is still free. Refusing after the call would leave a
        // credential that exists at Cloudflare, is in nobody's hands, and
        // cannot be fetched again.
        if let Some(name) = &bound.creates {
            if !store::valid_service_name(name) {
                return refuse(
                    &record,
                    StoreError::BadServiceName(name.clone()).to_string(),
                    ErrorKind::BadRequest,
                );
            }
            if self.store.info(peer.uid, name).is_some() {
                return refuse(
                    &record,
                    format!(
                        "this operation would store the credential it creates as \
                         '{name}', and a credential is already stored under that \
                         name. Overwriting it would destroy a secret the provider \
                         will not show again, so this service will not do it. \
                         Remove the stored one with `apex secret remove {name}` \
                         if that is what you meant"
                    ),
                    ErrorKind::BadRequest,
                );
            }
        }

        // §13.8, and the last thing that can refuse for free.
        //
        // AFTER the pin and after the `creates` check, because both of those
        // cost nothing and an approval costs the owner's attention: refusing a
        // request for a host mismatch *after* spending the one approval they
        // gave would make them give another. BEFORE the value is read, for the
        // ordinary reason every other check is — the credential is not touched
        // for a request that is not going to happen.
        //
        // Spent on commit and not on success. `perform` failing does not put
        // the approval back: an agent that could burn a failed deploy and keep
        // the approval could retry until something worked, and the owner
        // approved one deployment rather than one successful deployment. The
        // audit line says which it was.
        if let Some(why) = bound.approval.why() {
            record.approval_policy = APPROVAL_REQUIRED.to_string();
            if let Err(unspendable) = self.spend_approval(
                peer,
                &project,
                &record.provider,
                op,
                &record.resource,
                now,
            ) {
                let reason = if unspendable.is_empty() {
                    format!(
                        "{why}.\nThis machine has no approval outstanding for it, \
                         so it has not run."
                    )
                } else {
                    unspendable
                };
                return refuse(&record, reason, ErrorKind::PermissionDenied);
            }
            record.approval_policy = OWNER.to_string();
        }

        // Every check has passed. Only now is the value read.
        let stored = match self.store.value(peer.uid, &record.provider) {
            Ok(v) => v,
            Err(e) => return refuse(&record, e.to_string(), ErrorKind::NoSuchService),
        };

        // §13.4: prefer a short-lived credential where the provider has one.
        // The minted value is used INSTEAD of the stored one and scrubbed
        // alongside it — a temporary token is still a credential, and §13.4 is
        // explicit that it is not handed to the agent either.
        //
        // Four answers, not two, and the framework takes a different branch for
        // each. Three of them come back here as a *reason* and the stored
        // credential is used, which is what this service did before any of this
        // existed — the change is that the trail now says which of the three it
        // was. "The far side would not issue a narrower credential" and "there
        // is no narrower credential to issue" are different facts about the
        // owner's account, and an operator who cannot tell them apart cannot
        // act on either.
        let minted = match backend.mint(&req, &bound, &stored) {
            Ok(minted) => minted,
            // Scrubbed, like every refusal from here on. See the note below.
            Err(e) => {
                let reason = scrub_all(&e.to_string(), &[Some(&stored)]);
                return refuse(&record, reason, kind_of(&e));
            }
        };
        let narrowing = minted.as_str().to_string();
        let mut narrowing_detail = minted.reason().map(str::to_string);
        let (presented, lease) = match &minted {
            provider::Minted::Narrowed { value, lease } => (value, Some(lease.clone())),
            provider::Minted::NoNarrowerForm(_)
            | provider::Minted::Denied(_)
            | provider::Minted::CouldNotRun(_) => (&stored, None),
        };

        let mut performed = backend.perform(&req, &bound, presented);

        // §13.4 says *prefer* a short-lived credential. A preference that
        // turns a working operation into an authentication failure is not one,
        // and that is exactly what this was doing.
        //
        // Measured against `api.cloudflare.com` on 2026-09-12: a token
        // Cloudflare had just issued was accepted immediately by the account
        // and Workers endpoints and refused by D1's — HTTP 401, four attempts
        // out of four — while the same operation on the stored credential
        // succeeded. A minted credential is not usable everywhere the instant
        // it is issued, and nothing here had allowed for that.
        //
        // Three things about this loop are deliberate:
        //
        // * **It runs only when the credential was minted.** A 401 on the
        //   stored credential is the owner's answer and repeating the question
        //   does not change it. `lease.is_some()` is the whole condition.
        // * **It retries the MINTED credential, and never falls back to the
        //   stored one.** Widening silently would undo the narrowing at the
        //   first hiccup, and a project that set `temporary_credentials =
        //   "require"` would be run on the broad token by a code path it never
        //   agreed to. The escape hatch is a line in the project's own file.
        // * **Only the provider decides what a refusal of the credential
        //   looks like**, because only it can read its own far side. The
        //   framework must not parse an output string it did not compose.
        let mut attempt = 1;
        if lease.is_some() {
            while attempt <= NARROWED_REFUSAL_RETRIES
                && matches!(&performed, Ok(out) if backend.credential_refused(out))
            {
                std::thread::sleep(self.refusal_pause * attempt);
                performed = backend.perform(&req, &bound, presented);
                attempt += 1;
            }
            if matches!(&performed, Ok(out) if backend.credential_refused(out)) {
                narrowing_detail = Some(format!(
                    "the short-lived credential was minted and the far side \
                     refused it on all {attempt} attempts. What this operation \
                     reports is that refusal and not an answer about the \
                     operation itself. A project that would rather spend the \
                     stored credential says so in its own file, with \
                     `temporary_credentials = \"off\"` for this provider"
                ));
            } else if attempt > 1 {
                narrowing_detail = Some(format!(
                    "the short-lived credential was refused while it was \
                     seconds old and accepted on attempt {attempt}: a minted \
                     credential is not usable everywhere the instant it is \
                     issued"
                ));
            }
        }

        // The lease ends here, and it ends on BOTH paths out of `perform`.
        //
        // A minted credential that outlives a failed operation is the one this
        // whole section exists to avoid: the failure is often exactly the case
        // where something went wrong enough to be worth not leaving a spendable
        // credential behind. So the revoke happens before the error is even
        // looked at, and a revoke that could not be done is recorded rather
        // than dropped — an expiry is a backstop, and between here and it the
        // credential is still good.
        if let Some(lease) = &lease {
            if let Err(why) = backend.revoke(&req, &bound, &stored, lease) {
                let why = scrub_all(&why, &[Some(&stored), Some(presented)]);
                narrowing_detail = Some(format!(
                    "the short-lived credential was used but could not be \
                     revoked, so it stands until it expires at {} (unix ms): \
                     {why}",
                    lease.expires_ms
                ));
            }
        }

        let out = match performed {
            Ok(out) => out,
            // A provider is not supposed to put a credential in an error, and
            // one that does is not hypothetical: the natural way to write
            // `perform` is to hand back whatever the tool said, and what a
            // tool says about a failed request is often the request. Before
            // this, `refuse` was the one path out of `use_capability` that ran
            // no scrub — everything else either cannot carry a value
            // (`Response` does not implement it) or goes through `scrub_all`
            // below — so an `Err` raised after the value was read at the line
            // above put the credential in the reply AND in the audit trail.
            // Refusals before that point cannot: there is nothing read yet.
            Err(e) => {
                let reason = scrub_all(&e.to_string(), &[Some(&stored), minted.value()]);
                // Not `refuse`: an operation that failed after a credential was
                // presented is the one refusal that has a §13.4 outcome to
                // report, and reporting it only on the successful path would
                // leave the trail silent about exactly the runs worth reading.
                let (word, detail) = spend.borrow().clone();
                self.record(AuditLine {
                    reason: Some(reason.clone()),
                    narrowing: narrowing.clone(),
                    narrowing_detail: narrowing_detail.clone(),
                    spend: word,
                    spend_detail: detail,
                    ..AuditLine::from_record(
                        &audit_id,
                        AuditEvent::Refused,
                        peer.uid,
                        peer.pid,
                        &record,
                    )
                });
                return Response::error(kind_of(&e), reason);
            }
        };
        // Three credentials in play now, not two: the stored one, a minted one,
        // and one this operation may just have CREATED. The third is scrubbed
        // here for the same reason the other two are — a provider that puts a
        // `client_secret` in its own output is not hypothetical, because the
        // natural way to write such an operation is to hand back the reply, and
        // for these operations the reply *is* the secret.
        let mut output = scrub_all(
            &out.output,
            &[
                Some(&stored),
                minted.value(),
                out.created.as_ref().map(|c| &c.value),
            ],
        );

        let (spend_word, spend_detail) = spend.borrow().clone();
        self.record(AuditLine {
            endpoint: Some(endpoint.clone()),
            exit_code: Some(out.code),
            detail: bound.detail.clone(),
            narrowing: narrowing.clone(),
            narrowing_detail: narrowing_detail.clone(),
            spend: spend_word,
            spend_detail,
            ..AuditLine::from_record(&audit_id, AuditEvent::Used, peer.uid, peer.pid, &record)
        });

        // §13.10's other half: *"the agent receives handles/capabilities, not
        // plaintext secrets"*. The provider could not have done this itself —
        // `Bind` deliberately gives it no way to reach the store — so the
        // framework is where a created credential is kept, and therefore the
        // one place it cannot be forgotten.
        let mut code = out.code;
        match (bound.creates.as_deref(), out.created) {
            (Some(name), Some(created)) => match self.keep(peer, name, created) {
                Ok(note) => output.push_str(&note),
                // The operation HAPPENED. Saying so plainly matters more than
                // the exit code: somewhere there is now a credential nobody
                // holds, and the only person who can clean that up is the one
                // reading this sentence.
                Err(why) => {
                    code = 1;
                    output.push_str(&format!(
                        "\napex: this operation succeeded and the credential it \
                         created COULD NOT BE STORED as '{name}': {why}. The \
                         provider issues that secret once, so it is now lost — \
                         delete what this created at the provider and run it \
                         again."
                    ));
                }
            },
            // A provider that hands back a credential without having declared
            // it would. Nothing is stored and nothing is returned: the value is
            // dropped here, which loses it, and losing it is the right end for
            // a secret that arrived through a path this service did not check.
            (None, Some(_)) => {
                code = 1;
                output.push_str(
                    "\napex: this provider returned a credential without \
                     declaring that it creates one, so there was no name to \
                     store it under and no check that the name was free. It has \
                     been discarded rather than handed over.",
                );
            }
            (_, None) => {}
        }

        Response::Performed {
            record: Box::new(record),
            endpoint,
            exit_code: code,
            output,
        }
    }

    /// Put a credential a brokered operation just created into the store.
    ///
    /// §13.10, and the narrow reading of it: the secret goes where the owner's
    /// other secrets go, under a name the operation declared before it ran, and
    /// what comes back is that name. The value never enters a [`Response`] —
    /// it cannot, `SecretValue` is not `Serialize` — and it is not returned
    /// here either.
    ///
    /// The host is the pin every future use of this credential will be held to,
    /// which is why a provider that cannot name an honest one must not create a
    /// credential at all.
    fn keep(
        &self,
        peer: Peer,
        name: &str,
        created: provider::Created,
    ) -> Result<String, String> {
        if created.host.is_empty()
            || created.host.len() > 253
            || !created.host.chars().all(valid_host_char)
        {
            return Err(format!(
                "'{}' is not a host name to pin it to",
                created.host.escape_debug()
            ));
        }
        let info = ServiceInfo {
            service: name.to_string(),
            host: created.host.to_ascii_lowercase(),
            scheme: created.scheme.clone(),
            // The half of a service token that is not a secret. Stored beside
            // the half that is, because a `client_id` on its own is useless and
            // a `client_secret` on its own cannot be presented.
            username: created
                .username
                .clone()
                .unwrap_or_else(|| "x-access-token".to_string()),
            path: String::new(),
            auth: "bearer".to_string(),
            port: None,
            added: store::now_ms(),
        };
        // `put` refuses a scheme that is not https outside loopback, so a
        // credential this daemon creates gets the same protection as one the
        // owner typed in.
        self.store
            .put(peer.uid, &info, &created.value)
            .map_err(|e| e.to_string())?;
        self.record(AuditLine::administrative(
            &self.next_audit_id(),
            AuditEvent::Stored,
            peer.uid,
            peer.pid,
            name,
            &format!(
                "credential created by a brokered operation, for {}://{}",
                info.scheme, info.host
            ),
        ));
        Ok(format!(
            "\napex: the credential this created is stored as '{name}', pinned to \
             {}://{}. It is not in this reply and cannot be read back out of \
             this service; `apex secret list` shows that it is there.",
            info.scheme, info.host
        ))
    }
}

/// How a request was authorised, or why it was not.
///
/// The `&'static str` is §11's `approval_policy`. `grant` — a standing
/// per-project grant — is the only one this build reaches; P0-005's
/// session-scoped and break-glass answers are more variants of this, decided in
/// [`Service::decide`] and nowhere else.
enum Decision {
    Allowed(&'static str),
    Refused(String),
}

/// What `approval_policy` says before [`Service::decide`] has said anything.
///
/// Every line the refusal path writes carries this, so a trail can be read as
/// "these requests were never authorised" rather than as a mix of the daemon's
/// answers and the caller's claims.
const UNDECIDED: &str = "undecided";

/// Which protocol error a provider's refusal is.
///
/// A provider that could only say "error" would make every failure look like a
/// bug in APEX. `NoSuchResource` is a caller's typo, `Refused` is a policy the
/// caller can read, `Failed` is ours.
fn kind_of(e: &provider::ProviderError) -> ErrorKind {
    match e {
        provider::ProviderError::NoSuchResource(_) => ErrorKind::BadRequest,
        provider::ProviderError::Refused(_) => ErrorKind::PermissionDenied,
        provider::ProviderError::Failed(_) => ErrorKind::Internal,
    }
}

/// Remove every credential in play from anything on its way back to the caller.
///
/// In the framework rather than in each provider, so a provider added later
/// cannot forget. It is defence in depth either way: the reply type cannot
/// carry a `SecretValue`, and a provider is not supposed to print one — this is
/// for the case where a provider's own tooling embeds the credential in an
/// error message, which git does when a URL of the form
/// `https://user:token@host/…` fails.
fn scrub_all(text: &str, values: &[Option<&SecretValue>]) -> String {
    let mut out = text.to_string();
    for value in values.iter().flatten() {
        if let Some(token) = value.as_str() {
            out = broker::scrub(&out, token);
        }
    }
    out
}


/// What an `add` was asked to store, before any of it has been judged.
///
/// A struct rather than seven parameters: an endpoint is one thing described
/// seven ways, and a call site that passed `path` where `scheme` goes would
/// compile.
/// What an `approve` was asked to record, before any of it has been judged.
///
/// A struct rather than six parameters, for [`NewService`]'s reason: a call
/// site that passed `operation` where `resource` goes would compile, and the
/// consequence would be an approval that matches nothing — which looks exactly
/// like an approval that was spent.
pub struct NewApproval<'a> {
    pub project: &'a str,
    pub service: &'a str,
    /// Any spelling the registry resolves; the canonical id is what is stored.
    pub operation: &'a str,
    /// Exactly what the operation will name. Empty for one that names nothing.
    pub resource: &'a str,
    /// How long it may be spent for. `None` is [`store::APPROVAL_TTL_MS`].
    pub ttl_ms: Option<u64>,
    /// Take one back instead of giving one.
    pub withdraw: bool,
}

pub struct NewService<'a> {
    pub service: &'a str,
    pub host: &'a str,
    pub scheme: &'a str,
    pub username: Option<&'a str>,
    pub path: &'a str,
    pub auth: Option<&'a str>,
    pub port: Option<u16>,
}

fn valid_host_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_')
}

/// Whether `*` (every project) is a safe key for this operation.
///
/// **The operation's own declaration answers this**, and nothing here computes
/// it. That is the whole of the fix, and it took two wrong shapes to get here:
///
/// 1. P1-018 gated `--everywhere` on [`OperationSpec::names_nothing`],
///    reasoning that an operation naming nothing "can only ever reach the
///    endpoint pinned when its credential was stored". P1-002 landed the
///    counterexample in the same integration round: `cloudflare.account.read`
///    declares no resource and no parameters and still resolves its account out
///    of the project's own `apex.toml`. A rule over the declaration cannot see
///    what a provider's `bind` reads.
/// 2. The integration fix was `names_nothing() && id == "mcp.request"` — an
///    allow-list. Fail-closed, and **silent**: the next provider to declare an
///    operation like this gets the safe answer, the registry test keeps
///    passing, and nobody is ever asked the question. Safe by accident is not
///    safe by design.
///
/// [`OperationSpec::same_everywhere`] is the question, asked of the one party
/// that can answer it. There is no `Default` for `OperationSpec` and nothing
/// constructs one with `..`, so an operation added later does not compile until
/// its author has stated which of the two it is; and
/// [`apex_secret_core::operation::ProviderSpec::validate`] refuses the
/// incoherent half of the claim at registration, so this can read the field
/// alone.
pub(crate) fn may_be_granted_everywhere(op: &'static OperationSpec) -> bool {
    op.same_everywhere
}

fn refuse_store(e: StoreError) -> Response {
    let kind = match e {
        StoreError::NoSuchService(_) => ErrorKind::NoSuchService,
        StoreError::Io(_) => ErrorKind::Internal,
        _ => ErrorKind::BadRequest,
    };
    Response::error(kind, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    const SENTINEL: &str = "apex-sentinel-7f3a91c4-do-not-leak";

    fn temp_service(tag: &str) -> (Service, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "apex-secretd-svc-{}-{tag}-{}",
            std::process::id(),
            store::now_ms()
        ));
        std::fs::remove_dir_all(&dir).ok();
        let store = Store::new(dir.clone());
        let registry =
            crate::providers::default_registry(store.run_dir()).expect("the shipped registry");
        (Service::new(store, false, registry), dir)
    }

    /// The audit trail of a temp service, by path rather than through the
    /// service: the daemon has no reason to expose its own store, and a test
    /// that needs an accessor added for it is a test shaping the API.
    fn trail(dir: &Path) -> PathBuf {
        Store::new(dir.to_path_buf()).audit_path()
    }


    /// The seven fields an `add` takes, for a test that cares about two.
    fn demo_service<'a>(service: &'a str, host: &'a str, scheme: &'a str) -> NewService<'a> {
        NewService {
            service,
            host,
            scheme,
            username: None,
            path: "",
            auth: None,
            port: None,
        }
    }

    fn me() -> Peer {
        // Safe: getuid/getgid cannot fail.
        Peer {
            pid: std::process::id() as libc::pid_t,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        }
    }

    fn record(provider: &str, cap: &str, remote: &str, project: &str) -> CapabilityRecord {
        let mut rec = CapabilityRecord::new(provider, cap, remote);
        rec.project = Some(project.to_string());
        rec
    }

    /// A peer for a **real** second account on this machine.
    ///
    /// `me().uid + 1` is the obvious way to write "somebody else" and it is the
    /// wrong one, which cost `one_account_cannot_reach_another_accounts_
    /// credentials` its meaning for as long as it existed. `use_capability`
    /// looks the caller's account up in the passwd database *before* it looks
    /// in the store:
    ///
    /// ```text
    ///   let Some(owner) = broker::owner(peer.uid) else { …PermissionDenied… };
    ///   let Some(info)  = self.store.info(peer.uid, …) else { …NoSuchService… };
    /// ```
    ///
    /// Measured on this laptop on 2026-09-12: `id -u` is 1000 and there is no
    /// uid 1001 in `getent passwd` — the only account at or above 1000 is
    /// `andre`. So the refusal that test read as isolation was "uid 1001 is not
    /// an account on this machine", raised one step before the store was
    /// consulted at all. Permission denied is not absence: the credential
    /// namespace was never reached, and the assertion would have stayed green
    /// with the per-uid store ripped out.
    ///
    /// root always exists, so it is the second account, and it is also the one
    /// worth naming: on a shared machine the account that must not quietly
    /// inherit another's credential is the administrator's.
    fn another_real_account() -> Peer {
        let me = me();
        let other = if me.uid == 0 { 65534 } else { 0 };
        assert!(
            broker::owner(other).is_some(),
            "uid {other} must be a real account for this test to test anything"
        );
        Peer { uid: other, ..me }
    }

    #[test]
    fn the_other_account_in_these_tests_is_a_real_one() {
        // The guard on the guard. If `another_real_account` ever stops
        // resolving, the two tests below would go on passing while asserting
        // the account lookup instead of the isolation — which is the exact
        // failure they were written to replace, so it is pinned here in one
        // place rather than discovered again.
        let other = another_real_account();
        assert_ne!(other.uid, me().uid);
        assert!(broker::owner(other.uid).is_some());
        assert!(broker::owner(me().uid).is_some(), "our own uid must resolve");
    }

    #[test]
    fn another_account_is_refused_because_the_credential_is_absent_not_because_it_cannot_look() {
        // Criterion 1's per-user secrets, asserted where it is decided rather
        // than where it is convenient. Both accounts are real, so the passwd
        // lookup passes for both and the ONLY thing left to refuse the second
        // one is the per-uid store namespace.
        let (svc, dir) = temp_service("uid-absent");
        let mine = me();
        let theirs = another_real_account();
        svc.add(
            mine,
            demo_service("demo", "github.com", "https"),
            SecretValue::new(SENTINEL.into()),
        );

        let resp = svc.use_capability(
            theirs,
            record("demo", "git-fetch", "origin", "/tmp/p"),
            Vec::new(),
        );
        let (kind, message) = resp.as_error().expect("must refuse");
        assert_eq!(
            kind,
            ErrorKind::NoSuchService,
            "refused, but not by the store: {message}"
        );
        assert!(
            message.contains("demo"),
            "the refusal must name what was absent: {message}"
        );
        // And the refusal is about absence in THEIR namespace, not about who
        // they are: nothing in it mentions the account.
        assert!(
            !message.contains("not an account"),
            "this is the account lookup answering, one step too early: {message}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_grant_on_a_shared_project_path_does_not_cross_accounts() {
        // The shared-machine case, which is what P2-016 is about and what a
        // per-uid store is not on its own enough to settle. A grant is keyed by
        // PROJECT PATH, and a project path is common ground: /srv/build,
        // /var/tmp/ci, a shared checkout. Two accounts naming the same
        // directory must not share the permission attached to it.
        //
        // Both accounts hold a credential called `demo` here, deliberately, so
        // the service-existence step passes for both and the request reaches
        // the grant decision. That is what makes the refusal below specifically
        // about the grant and not a second reading of the previous test.
        let (svc, dir) = temp_service("uid-grants");
        let mine = me();
        let theirs = another_real_account();
        const SHARED: &str = "/srv/shared";

        for who in [mine, theirs] {
            svc.add(
                who,
                demo_service("demo", "github.com", "https"),
                SecretValue::new(SENTINEL.into()),
            );
        }
        // `grant` answers with the whole grant map rather than `Ok`, so the
        // caller sees what it now has; the shared path must be in it.
        match svc.grant(mine, SHARED, "demo", "git-fetch", false) {
            Response::Grants { projects } => {
                assert!(projects.contains_key(SHARED), "{projects:?}");
            }
            other => panic!("{other:?}"),
        }

        // Mine works on the shared path...
        match svc.grants(mine) {
            Response::Grants { projects } => {
                assert!(projects.contains_key(SHARED), "{projects:?}");
            }
            other => panic!("{other:?}"),
        }
        // ...and theirs sees no grant on it at all.
        match svc.grants(theirs) {
            Response::Grants { projects } => {
                assert!(
                    projects.is_empty(),
                    "another account's grant on a shared path was visible: {projects:?}"
                );
            }
            other => panic!("{other:?}"),
        }

        let resp = svc.use_capability(
            theirs,
            record("demo", "git-fetch", "origin", SHARED),
            Vec::new(),
        );
        let (kind, message) = resp.as_error().expect("must refuse");
        assert_eq!(kind, ErrorKind::PermissionDenied, "{message}");
        assert!(
            message.contains("not granted for this project"),
            "refused, but not by the grant decision — so this test would stay \
             green with per-uid grants removed: {message}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_grant_already_on_disk_under_an_old_spelling_still_works_and_can_be_removed() {
        // The compatibility claim, against a grants.json the way P0-002 wrote
        // one — written straight to the store rather than through `grant`,
        // which canonicalises on the way in and so could never produce a
        // legacy key. Andre's machine has exactly these.
        let (svc, dir) = temp_service("legacy");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"x".to_vec()));

        let store = Store::new(dir.clone());
        let mut grants = apex_secret_core::store::Grants::default();
        grants.allow("/tmp/p", "demo", "git-fetch");
        store.save_grants(peer.uid, &grants).expect("save");

        // It matches under the name the request now uses. The operation then
        // fails for its own reasons — /tmp/p is not a repository — and that is
        // the point: it got PAST the grant check.
        let past = svc.use_capability(peer, record("demo", "git.fetch", "origin", "/tmp/p"), Vec::new());
        assert!(
            !past.as_error().is_some_and(|(_, m)| m.contains("not granted")),
            "a grant on disk under the old name stopped matching: {past:?}"
        );

        // The contrast, so the assertion above cannot pass by accident: with
        // nothing granted, the same request is refused for the grant.
        let (empty, empty_dir) = temp_service("legacy-contrast");
        empty.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"x".to_vec()));
        assert!(
            empty
                .use_capability(peer, record("demo", "git.fetch", "origin", "/tmp/p"), Vec::new())
                .as_error()
                .is_some_and(|(_, m)| m.contains("not granted")),
            "the ungranted case must be refused for the grant"
        );
        std::fs::remove_dir_all(&empty_dir).ok();

        // ...and it can be withdrawn, under either spelling. Without this the
        // owner is told the grant is not there while it goes on working.
        let reply = svc.grant(peer, "/tmp/p", "demo", "git.fetch", true);
        assert!(reply.as_error().is_none(), "revoke refused: {reply:?}");
        assert_eq!(
            svc.grants(peer),
            Response::Grants {
                projects: Default::default()
            },
            "the legacy key survived a revoke"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_refusal_never_records_an_approval_the_caller_claimed() {
        // §11's `approval_policy` is the daemon's answer. A request refused
        // before `decide` has no answer, and a trail that carried the caller's
        // claim there would say `owner` on a line nobody authorised.
        let (svc, dir) = temp_service("undecided");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"x".to_vec()));
        let mut rec = record("demo", "git.fetch", "origin", "/tmp/p");
        rec.approval_policy = "owner".into();
        svc.use_capability(peer, rec, Vec::new());

        let line = audit::tail(&trail(&dir), 10)
            .into_iter()
            .find(|l| l.event == AuditEvent::Refused)
            .expect("a refusal was recorded");
        assert_eq!(line.approval_policy, UNDECIDED, "the caller's claim reached the trail");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hello_reports_the_vocabulary_and_whether_the_store_is_protected() {
        let (svc, dir) = temp_service("hello");
        match svc.hello() {
            Response::Hello {
                version,
                capabilities,
                vocabulary,
                protected,
            } => {
                assert_eq!(version, apex_secret_core::protocol::PROTOCOL_VERSION);
                // The registry's canonical ids, never an alias: the list is
                // what `apex secret capabilities` prints, and advertising a
                // spelling on its way out teaches people to use it.
                assert!(capabilities.contains(&"git.fetch".to_string()), "{capabilities:?}");
                assert!(!capabilities.contains(&"git-fetch".to_string()), "{capabilities:?}");
                // And with enough beside each id that `apex secret
                // capabilities` needs no list of its own.
                let fetch = vocabulary.iter().find(|o| o.id == "git.fetch").expect("in vocabulary");
                assert!(!fetch.summary.is_empty());
                assert_eq!(fetch.effect, "read");
                assert_eq!(vocabulary.len(), capabilities.len());
                // A daemon running as an ordinary user must say so rather than
                // implying a boundary it does not have.
                assert!(!protected);
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_stored_credential_is_never_in_any_reply() {
        // The criterion, against the shipped verbs rather than against
        // hand-built responses: store a sentinel, then serialise every reply
        // the service can produce for a caller that has one.
        let (svc, dir) = temp_service("noleak");
        let peer = me();
        assert_eq!(
            svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(SENTINEL.into())
            ),
            Response::Ok
        );
        svc.grant(peer, "/tmp/p", "demo", "git-fetch", false);

        let replies = vec![
            svc.hello(),
            svc.list(peer),
            svc.grants(peer),
            svc.audit(peer, 100, None),
            // A use that gets as far as it can without a repository.
            svc.use_capability(peer, record("demo", "git-fetch", "origin", "/tmp/p"), Vec::new()),
            // ...and one that is refused early.
            svc.use_capability(peer, record("demo", "git-push", "origin", "/tmp/p"), Vec::new()),
            svc.remove(peer, "demo"),
        ];
        for reply in &replies {
            let text = serde_json::to_string(reply).expect("serialise");
            assert!(
                !text.contains(SENTINEL),
                "{} carried the credential: {text}",
                reply.variant()
            );
        }
        // And the trail did not either.
        let trail = std::fs::read_to_string(trail(&dir)).unwrap_or_default();
        assert!(!trail.contains(SENTINEL), "{trail}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_credential_grants_nothing_by_itself() {
        let (svc, dir) = temp_service("nogrant");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"x".to_vec()));
        assert_eq!(
            svc.grants(peer),
            Response::Grants {
                projects: Default::default()
            }
        );
        let resp = svc.use_capability(peer, record("demo", "git-fetch", "origin", "/tmp/p"), Vec::new());
        let (kind, message) = resp.as_error().expect("refused");
        assert_eq!(kind, ErrorKind::PermissionDenied);
        assert!(message.contains("not granted"), "{message}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_grant_is_per_capability_and_per_project() {
        let (svc, dir) = temp_service("perproject");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"x".to_vec()));
        svc.grant(peer, "/tmp/p", "demo", "git-fetch", false);

        // Another capability in the same project.
        assert!(svc
            .use_capability(peer, record("demo", "git-push", "origin", "/tmp/p"), Vec::new())
            .as_error()
            .is_some_and(|(_, m)| m.contains("not granted")));
        // The same capability in another project.
        assert!(svc
            .use_capability(peer, record("demo", "git-fetch", "origin", "/tmp/q"), Vec::new())
            .as_error()
            .is_some_and(|(_, m)| m.contains("not granted")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_grant_for_an_unknown_credential_is_refused_rather_than_stored() {
        // A typo that produced a grant would silently never match, and the
        // person who made it would be told the capability was allowed.
        let (svc, dir) = temp_service("typo");
        let peer = me();
        let resp = svc.grant(peer, "/tmp/p", "nosuch", "git-fetch", false);
        assert!(resp.as_error().is_some_and(|(k, _)| k == ErrorKind::NoSuchService));
        assert_eq!(
            svc.grants(peer),
            Response::Grants {
                projects: Default::default()
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_vocabulary_is_closed_at_the_grant_and_at_the_use() {
        let (svc, dir) = temp_service("closed");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"x".to_vec()));
        // `cloudflare.account.delete` is deliberately not a §13.2 name and
        // never will be: this list wants a well-formed id that no provider
        // declares, and it used to hold `cloudflare.dns.delete`, which stopped
        // being one the day P1-007 implemented it. A name taken from the
        // unimplemented end of §13.2 is a trap for whoever implements it next.
        for evil in ["exec", "sh", "git-clone", "curl", "git.clone", "cloudflare.account.delete"] {
            let resp = svc.grant(peer, "/tmp/p", "demo", evil, false);
            assert!(
                resp.as_error()
                    .is_some_and(|(_, m)| m.contains("not an operation")),
                "'{evil}' was grantable: {resp:?}"
            );
        }
        // And an operation a registered provider DOES offer is grantable under
        // either spelling, and stored under the canonical one.
        for spelling in ["git-fetch", "git.fetch"] {
            let resp = svc.grant(peer, "/tmp/p", "demo", spelling, false);
            assert_eq!(
                resp,
                Response::Grants {
                    projects: std::collections::BTreeMap::from([(
                        "/tmp/p".to_string(),
                        vec!["demo:git.fetch".to_string()]
                    )])
                },
                "{spelling} was not canonicalised"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_use_without_a_project_cannot_match_a_grant() {
        let (svc, dir) = temp_service("noproject");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"x".to_vec()));
        let mut rec = record("demo", "git-fetch", "origin", "/tmp/p");
        rec.project = None;
        assert!(svc
            .use_capability(peer, rec, Vec::new())
            .as_error()
            .is_some_and(|(_, m)| m.contains("names no project")));

        // ...and neither can a relative one, which would otherwise be resolved
        // against the DAEMON's working directory.
        let mut rec = record("demo", "git-fetch", "origin", "/tmp/p");
        rec.project = Some("relative/p".into());
        assert!(svc.use_capability(peer, rec, Vec::new()).as_error().is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_origin_label_that_could_reshape_the_trail_is_refused() {
        // The audit trail is one JSON object per line and an administrator
        // greps it. This daemon cannot verify WHERE a request came from — the
        // connection it sees belongs to apex-agentd — but it can refuse a
        // value that is not a label at all.
        let (svc, dir) = temp_service("originshape");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"x".to_vec()));
        svc.grant(peer, "/tmp/p", "demo", "git-fetch", false);

        for bad in ["local terminal", "Local-Terminal", "local\nterminal", ""] {
            let mut rec = record("demo", "git-fetch", "origin", "/tmp/p");
            rec.request_origin = bad.to_string();
            let resp = svc.use_capability(peer, rec, Vec::new());
            assert!(
                resp.as_error()
                    .is_some_and(|(k, m)| k == ErrorKind::BadRequest
                        && m.contains("request_origin")),
                "'{}' was accepted: {resp:?}",
                bad.escape_debug()
            );
        }
        // The same rule for how the origin was reached.
        let mut rec = record("demo", "git-fetch", "origin", "/tmp/p");
        rec.origin_source = "observed by me".into();
        assert!(svc
            .use_capability(peer, rec, Vec::new())
            .as_error()
            .is_some_and(|(_, m)| m.contains("origin_source")));

        // ...and a well-formed pair gets past this check to the next one. The
        // refusal that follows is about the repository, not about the label —
        // matched on the FIELD names, because the remote in this fixture is
        // itself called `origin` and a looser match passes for the wrong
        // reason.
        let mut rec = record("demo", "git-fetch", "origin", "/tmp/p");
        rec.request_origin = "claude-remote-control".into();
        rec.origin_source = "declared".into();
        let resp = svc.use_capability(peer, rec, Vec::new());
        let (_, message) = resp.as_error().expect("no repository at /tmp/p");
        assert!(!message.contains("request_origin"), "{message}");
        assert!(!message.contains("origin_source"), "{message}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_origin_a_caller_forwarded_reaches_the_trail_unchanged() {
        // §7's field is only useful if it survives to the record an owner
        // reads. Nothing here verifies it — that is written down — but a value
        // the daemon silently dropped would be worse than one it never had.
        let (svc, dir) = temp_service("origintrail");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"x".to_vec()));
        let mut rec = record("demo", "git-fetch", "origin", "/tmp/p");
        rec.request_origin = "claude-remote-control".into();
        rec.origin_source = "inherited".into();
        svc.use_capability(peer, rec, Vec::new());

        let refused = audit::tail(&Store::new(dir.clone()).audit_path(), 10)
            .into_iter()
            .find(|l| l.event == AuditEvent::Refused)
            .expect("the refusal is in the trail");
        assert_eq!(refused.request_origin, "claude-remote-control");
        assert_eq!(refused.origin_source, "inherited");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_expired_or_over_long_request_is_refused() {
        let (svc, dir) = temp_service("expiry");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"x".to_vec()));
        svc.grant(peer, "/tmp/p", "demo", "git-fetch", false);

        let mut rec = record("demo", "git-fetch", "origin", "/tmp/p");
        rec.expiry = Some(1);
        assert!(svc
            .use_capability(peer, rec, Vec::new())
            .as_error()
            .is_some_and(|(_, m)| m.contains("expired")));

        let mut rec = record("demo", "git-fetch", "origin", "/tmp/p");
        rec.expiry = Some(store::now_ms() + MAX_EXPIRY_AHEAD_MS * 2);
        assert!(svc
            .use_capability(peer, rec, Vec::new())
            .as_error()
            .is_some_and(|(_, m)| m.contains("longer than this service")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn every_refusal_is_recorded_with_a_reason_and_an_id() {
        let (svc, dir) = temp_service("trail");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"x".to_vec()));
        svc.use_capability(peer, record("demo", "git-fetch", "origin", "/tmp/p"), Vec::new());

        let lines = audit::tail(&trail(&dir), 10);
        let refused: Vec<&AuditLine> = lines
            .iter()
            .filter(|l| l.event == AuditEvent::Refused)
            .collect();
        assert_eq!(refused.len(), 1, "{lines:#?}");
        let line = refused[0];
        assert!(!line.audit_id.is_empty());
        assert_eq!(line.provider, "demo");
        // The request named the operation by its old spelling; the trail says
        // the canonical one, so one operation cannot appear in an audit under
        // two names depending on how the caller typed it.
        assert_eq!(line.operation, "git.fetch");
        assert_eq!(line.resource, "origin");
        assert_eq!(line.uid, peer.uid);
        assert!(line.reason.as_deref().is_some_and(|r| r.contains("not granted")));
        // Nothing was contacted, so there is no endpoint to claim.
        assert_eq!(line.endpoint, None);
        assert_eq!(line.exit_code, None);

        // The store event is there too, and it is what `apex secret audit`
        // shows the owner.
        assert!(lines.iter().any(|l| l.event == AuditEvent::Stored));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn one_account_cannot_reach_another_accounts_credentials() {
        // A uid that is deliberately NOT an account. `list` and `audit` read
        // the store and the trail directly and never consult passwd, so they
        // are answered by the per-uid namespace whether or not the uid exists —
        // which is what makes this the right test for them, and what made it
        // the WRONG test for `use_capability`.
        //
        // That third assertion used to live here as `.as_error().is_some()`.
        // On this machine there is no uid 1001, so it was satisfied by "uid
        // 1001 is not an account on this machine", raised before the store was
        // opened: the isolation it named was never exercised. It moved to
        // `another_account_is_refused_because_the_credential_is_absent_not_
        // because_it_cannot_look`, which uses a real second account and pins
        // the refusal kind.
        let (svc, dir) = temp_service("uids");
        let mine = me();
        let theirs = Peer {
            uid: mine.uid.wrapping_add(1),
            ..mine
        };
        svc.add(mine, demo_service("demo", "github.com", "https"), SecretValue::new(SENTINEL.into()));

        assert_eq!(svc.list(theirs), Response::Services { services: vec![] });
        // Still filed, because the audit assertions below need a refused line
        // of theirs to exist — but it is no longer read as proof of isolation.
        assert!(svc
            .use_capability(theirs, record("demo", "git-fetch", "origin", "/tmp/p"), Vec::new())
            .as_error()
            .is_some());
        // The trail is filtered too, so it is not a way to learn what another
        // account has stored. The refusal above is in it — that line is theirs
        // — and nothing of mine is.
        match svc.audit(theirs, 100, None) {
            Response::Audit { entries } => {
                assert!(entries.iter().all(|l| l.uid == theirs.uid), "{entries:#?}");
                assert!(
                    !entries.iter().any(|l| l.event == AuditEvent::Stored),
                    "another account's `stored` line was visible: {entries:#?}"
                );
            }
            other => panic!("{other:?}"),
        }
        // ...and mine still shows it.
        match svc.audit(mine, 100, None) {
            Response::Audit { entries } => {
                assert!(entries.iter().any(|l| l.event == AuditEvent::Stored), "{entries:#?}")
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn removing_a_credential_takes_its_grants_with_it() {
        let (svc, dir) = temp_service("removegrants");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"x".to_vec()));
        svc.grant(peer, "/tmp/p", "demo", "git-fetch", false);
        assert_eq!(svc.remove(peer, "demo"), Response::Ok);
        assert_eq!(
            svc.grants(peer),
            Response::Grants {
                projects: Default::default()
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn http_is_refused_for_a_host_that_is_not_this_machine() {
        let (svc, dir) = temp_service("scheme");
        let peer = me();
        let resp = svc.add(peer, demo_service("demo", "github.com", "http"), SecretValue::new(b"x".to_vec()),
        );
        assert!(resp
            .as_error()
            .is_some_and(|(_, m)| m.contains("loopback")), "{resp:?}");
        assert!(Store::new(dir.clone()).info(peer.uid, "demo").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_host_name_is_validated_before_it_is_pinned() {
        let (svc, dir) = temp_service("host");
        let peer = me();
        for evil in ["", "a b", "a/b", "a\nb", "-"] {
            let resp = svc.add(peer, demo_service("demo", evil, "https"), SecretValue::new(b"x".to_vec()));
            if evil == "-" {
                // A single hyphen is a legal host character; it simply never
                // matches a remote. The framing characters are what matter.
                continue;
            }
            assert!(resp.as_error().is_some(), "'{}' was accepted", evil.escape_debug());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── §13.10: a credential an operation CREATES ───────────────────────────
    //
    // The seam these three tests exist for is small and easy to get subtly
    // wrong, so each one is written against a provider that behaves the way the
    // careless implementation would: it puts the secret it just issued straight
    // into its own output. A test whose fake provider politely withheld the
    // value could not tell a working scrub from a missing one.
    //
    // Four mutations were run against the arms these cover, one at a time, each
    // restored by copying the pristine file back so that cargo rebuilt rather
    // than measuring a mutant against a stale binary. Every one turns a named
    // test red:
    //
    // * the framework's scrub no longer given the created value, so the
    //   provider's own output hands it over —
    //   `a_credential_an_operation_creates_is_stored_and_never_comes_back`;
    // * `keep` reporting that it stored the credential without storing it —
    //   the same test, which is why that one asserts the stored BYTES and not
    //   merely that a service appeared;
    // * the collision check removed, so a second create destroys the first
    //   token's secret — `a_created_credential_never_overwrites_one_…`;
    // * an undeclared credential reported as a clean run —
    //   `a_provider_that_returns_a_credential_it_never_declared_…`.

    /// What the fake provider issues. Distinctive enough that finding it
    /// anywhere downstream means it travelled.
    const ISSUED: &str = "apex-issued-secret-6b41f0-do-not-leak";

    use apex_secret_core::operation::ProviderSpec;

    /// A provider that creates a credential, the way §13.10's Access service
    /// token and Tunnel credential do.
    struct Creator {
        /// Whether `perform` ran. A refusal that is supposed to happen BEFORE
        /// anything is issued has to be measured, not assumed.
        ran: std::sync::atomic::AtomicBool,
    }

    const CREATOR: ProviderSpec = ProviderSpec {
        id: "creator",
        summary: "issues credentials, so the framework's half of §13.10 can be tested",
        operations: &[
            OperationSpec {
                id: "creator.token.create",
                summary: "issue a token and leave it with this service rather than the caller",
                effect: apex_secret_core::operation::Effect::Write,
                resource: apex_secret_core::operation::ResourceKind::Name,
                params: &[],
                aliases: &[],
                same_everywhere: false,
            },
            OperationSpec {
                id: "creator.token.smuggle",
                summary: "hand back a credential without declaring one, which must not work",
                effect: apex_secret_core::operation::Effect::Write,
                resource: apex_secret_core::operation::ResourceKind::Name,
                params: &[],
                aliases: &[],
                same_everywhere: false,
            },
        ],
    };

    impl provider::Provider for Creator {
        fn spec(&self) -> &'static ProviderSpec {
            &CREATOR
        }

        fn bind(&self, req: &provider::Bind<'_>) -> Result<provider::Bound, provider::ProviderError> {
            Ok(provider::Bound {
                endpoint: provider::Endpoint {
                    scheme: "https".to_string(),
                    host: "api.example.test".to_string(),
                },
                detail: format!("issue a token for {}", req.resource),
                // The declaration the framework checks before anything runs —
                // except for `smuggle`, which deliberately does not make it.
                creates: match req.operation.id {
                    "creator.token.create" => Some(format!("issued-{}", req.resource)),
                    _ => None,
                },
                approval: provider::Approval::Standing,
            })
        }

        fn perform(
            &self,
            _req: &provider::Bind<'_>,
            _bound: &provider::Bound,
            _value: &SecretValue,
        ) -> Result<provider::Performed, provider::ProviderError> {
            self.ran.store(true, Ordering::SeqCst);
            Ok(provider::Performed {
                code: 0,
                // What the careless provider does: hands back the reply, and
                // for this operation the reply IS the secret.
                output: format!(r#"{{"client_id":"id-42","client_secret":"{ISSUED}"}}"#),
                created: Some(provider::Created {
                    host: "origin.example.test".to_string(),
                    scheme: "https".to_string(),
                    username: Some("id-42".to_string()),
                    value: SecretValue::new(ISSUED.as_bytes().to_vec()),
                }),
            })
        }
    }

    /// A service serving only [`Creator`], with a credential stored for it and
    /// both its operations granted in `/tmp/p`.
    fn creator_service(tag: &str) -> (Service, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "apex-secretd-creates-{}-{tag}-{}",
            std::process::id(),
            store::now_ms()
        ));
        std::fs::remove_dir_all(&dir).ok();
        let mut registry = Registry::new();
        registry
            .register(Box::new(Creator {
                ran: std::sync::atomic::AtomicBool::new(false),
            }))
            .expect("register");
        let service = Service::new(Store::new(dir.clone()), false, registry);
        let peer = me();
        service.add(
            peer,
            demo_service("creator", "api.example.test", "https"),
            SecretValue::new(b"a-stored-credential".to_vec()),
        );
        for op in ["creator.token.create", "creator.token.smuggle"] {
            service.grant(peer, "/tmp/p", "creator", op, false);
        }
        (service, dir)
    }

    #[test]
    fn a_credential_an_operation_creates_is_stored_and_never_comes_back() {
        // §13.10: "the agent receives handles/capabilities, not plaintext
        // secrets". Both halves are measured — that the caller did NOT get the
        // secret, and that it IS in the store, byte for byte. A build that
        // scrubbed the output and stored nothing would pass the first half and
        // lose the credential forever, which is worse than not implementing it.
        let (svc, dir) = creator_service("stored");
        let peer = me();
        let reply = svc.use_capability(peer, record("creator", "creator.token.create", "alpha", "/tmp/p"), Vec::new());

        let Response::Performed { output, exit_code, .. } = &reply else {
            panic!("{reply:?}");
        };
        assert_eq!(*exit_code, 0, "{output}");
        assert!(!output.contains(ISSUED), "the secret came back to the caller: {output}");
        assert!(output.contains("stored as 'issued-alpha'"), "{output}");
        assert!(output.contains("https://origin.example.test"), "{output}");

        // It is in the store, it is the real value, and it is pinned to the
        // host the provider named rather than to the one it was created at.
        let store = Store::new(dir.clone());
        let info = store.info(peer.uid, "issued-alpha").expect("stored");
        assert_eq!(info.host, "origin.example.test");
        assert_eq!(info.scheme, "https");
        assert_eq!(info.username, "id-42");
        let kept = store.value(peer.uid, "issued-alpha").expect("value");
        assert_eq!(kept.as_str(), Some(ISSUED), "a different secret was stored");

        // And the trail says a credential was stored, not merely that an
        // operation ran: this is the line an owner greps after an incident.
        let lines = audit::tail(&trail(&dir), 50);
        let stored = lines
            .iter()
            .find(|l| l.event == AuditEvent::Stored && l.provider == "issued-alpha")
            .expect("no `stored` line for the created credential");
        assert!(stored.detail.contains("created by a brokered operation"), "{stored:?}");
        assert!(
            !std::fs::read_to_string(trail(&dir)).unwrap_or_default().contains(ISSUED),
            "the secret is in the audit trail"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_created_credential_never_overwrites_one_that_is_already_stored() {
        // The provider issues such a secret ONCE. So the collision has to be
        // refused before the call, not after it: a refusal afterwards would
        // leave a credential that exists at the provider, is in nobody's hands,
        // and cannot be fetched again. `ran` is what proves the order.
        let (svc, dir) = creator_service("collision");
        let peer = me();
        svc.add(
            peer,
            demo_service("issued-alpha", "somewhere.example.test", "https"),
            SecretValue::new(b"the one that was already there".to_vec()),
        );

        let reply = svc.use_capability(peer, record("creator", "creator.token.create", "alpha", "/tmp/p"), Vec::new());
        let (kind, message) = reply.as_error().expect("a collision must be refused");
        assert_eq!(kind, ErrorKind::BadRequest, "{message}");
        assert!(message.contains("issued-alpha"), "{message}");
        assert!(message.contains("apex secret remove"), "{message}");

        // Nothing ran, so nothing was issued...
        let store = Store::new(dir.clone());
        assert_eq!(
            store.value(peer.uid, "issued-alpha").expect("value").as_str(),
            Some("the one that was already there"),
            "the stored credential was overwritten"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_provider_that_returns_a_credential_it_never_declared_has_it_discarded() {
        // The framework checks the NAME before the call. A provider that
        // produced a value without having declared one would slip past that
        // check, so the value is dropped rather than stored under a name nobody
        // vetted — and dropped rather than returned, which is the whole point.
        let (svc, dir) = creator_service("smuggle");
        let peer = me();
        let reply = svc.use_capability(peer, record("creator", "creator.token.smuggle", "beta", "/tmp/p"), Vec::new());

        let Response::Performed { output, exit_code, .. } = &reply else {
            panic!("{reply:?}");
        };
        assert!(!output.contains(ISSUED), "the smuggled secret came back: {output}");
        assert_eq!(*exit_code, 1, "a discarded credential is not a clean run: {output}");
        assert!(output.contains("discarded"), "{output}");
        assert!(
            Store::new(dir.clone()).info(peer.uid, "issued-beta").is_none(),
            "it was stored under a name nothing checked"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── §13.8's approval verb, at the framework's own level ─────────────────

    fn approval_of<'a>(project: &'a str, operation: &'a str, resource: &'a str) -> NewApproval<'a> {
        NewApproval {
            project,
            service: "demo",
            operation,
            resource,
            ttl_ms: None,
            withdraw: false,
        }
    }

    #[test]
    fn an_approval_cannot_be_given_for_every_project() {
        // An approval that applied wherever the agent happened to be standing
        // would be standing permission with a shorter life, which is precisely
        // the thing §13.8 says has to be written into the project file
        // instead. `*` is a valid grant key and must not be a valid approval
        // key, so the two paths are checked separately rather than sharing a
        // validator that would have had to allow it.
        let (svc, dir) = temp_service("approve-star");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"t".to_vec()));
        let reply = svc.approve(peer, approval_of(store::ANY_PROJECT, "git.push", "origin"));
        let (kind, message) = reply.as_error().expect("`*` is not a project");
        assert_eq!(kind, ErrorKind::BadRequest, "{message}");
        assert!(message.contains("absolute path"), "{message}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_approval_is_recorded_under_the_canonical_operation_id() {
        // The same property a grant has, and for the same reason: an alias is
        // an input this service accepts, never a spelling it stores. An
        // approval filed under `git-push` would stop matching the moment the
        // request arrived as `git.push`.
        let (svc, dir) = temp_service("approve-alias");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"t".to_vec()));
        let reply = svc.approve(peer, approval_of("/tmp/p", "git-push", "origin"));
        let Response::Approvals { pending } = reply else {
            panic!("{reply:?}");
        };
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].operation, "git.push", "an alias became a stored fact");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_approval_for_an_operation_nothing_offers_is_refused_rather_than_stored() {
        // The closed vocabulary, at this verb too. An approval for `exec`
        // would sit in the file looking like permission for something.
        let (svc, dir) = temp_service("approve-unknown");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"t".to_vec()));
        assert!(svc
            .approve(peer, approval_of("/tmp/p", "exec", "sh"))
            .as_error()
            .is_some());
        // And a resource the operation's own grammar refuses.
        let reply = svc.approve(peer, approval_of("/tmp/p", "git.push", "https://evil.example/x"));
        let (kind, message) = reply.as_error().expect("a URL is not a resource");
        assert_eq!(kind, ErrorKind::BadRequest, "{message}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_approval_for_a_credential_that_does_not_exist_is_refused() {
        // `grant`'s reason: otherwise a typo produces an approval that
        // silently never matches, and the owner believes they approved
        // something.
        let (svc, dir) = temp_service("approve-typo");
        let peer = me();
        let reply = svc.approve(peer, approval_of("/tmp/p", "git.push", "origin"));
        assert_eq!(
            reply.as_error().map(|(kind, _)| kind),
            Some(ErrorKind::NoSuchService)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_approval_that_would_outlive_the_bound_is_refused_and_not_shortened() {
        // `deploy::share`'s rule. Somebody who wrote a week meant a week, and
        // silently giving them a day would be this service deciding what they
        // meant — in the direction of MORE standing permission than they asked
        // for the life of, which is the wrong direction to guess in.
        let (svc, dir) = temp_service("approve-ttl");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"t".to_vec()));
        for bad in [0, store::MAX_APPROVAL_TTL_MS + 1] {
            let mut asked = approval_of("/tmp/p", "git.push", "origin");
            asked.ttl_ms = Some(bad);
            let reply = svc.approve(peer, asked);
            let (kind, message) = reply.as_error().unwrap_or_else(|| panic!("{bad} was accepted"));
            assert_eq!(kind, ErrorKind::BadRequest, "{message}");
        }
        let mut good = approval_of("/tmp/p", "git.push", "origin");
        good.ttl_ms = Some(store::MAX_APPROVAL_TTL_MS);
        assert!(svc.approve(peer, good).as_error().is_none(), "the bound itself is allowed");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn withdrawing_something_that_was_never_approved_says_so() {
        // A verb that answered "done" to a withdraw that withdrew nothing
        // would let somebody believe they had taken back an approval that is
        // still outstanding.
        let (svc, dir) = temp_service("approve-withdraw");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"t".to_vec()));
        let mut asked = approval_of("/tmp/p", "git.push", "origin");
        asked.withdraw = true;
        assert_eq!(
            svc.approve(peer, asked).as_error().map(|(kind, _)| kind),
            Some(ErrorKind::NoSuchService)
        );

        assert!(svc.approve(peer, approval_of("/tmp/p", "git.push", "origin")).as_error().is_none());
        let mut asked = approval_of("/tmp/p", "git.push", "origin");
        asked.withdraw = true;
        assert!(svc.approve(peer, asked).as_error().is_none());
        let Response::Approvals { pending } = svc.approvals(peer) else {
            panic!("not an approvals reply");
        };
        assert!(pending.is_empty(), "{pending:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn one_accounts_approvals_are_not_another_accounts() {
        // The store is per-uid and the daemon derives the uid from
        // SO_PEERCRED, so this is really a test that the approval path did not
        // introduce the one thing the rest of the store is built to prevent.
        let (svc, dir) = temp_service("approve-uid");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"t".to_vec()));
        assert!(svc.approve(peer, approval_of("/tmp/p", "git.push", "origin")).as_error().is_none());

        let other = Peer { uid: peer.uid.wrapping_add(1), ..peer };
        let Response::Approvals { pending } = svc.approvals(other) else {
            panic!("not an approvals reply");
        };
        assert!(pending.is_empty(), "another account saw this one's approvals: {pending:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_three_approval_policy_words_stay_distinct() {
        // `Approval::Standing` is not "no check" — it is "the grant that was
        // already checked is the answer". If the three words collapsed, the
        // trail could no longer tell a standing grant from an approval the
        // owner gave from a request that was refused for want of one, and
        // those are the only three things §11's field is for.
        assert_eq!(
            provider::Approval::Standing.why(),
            None,
            "a standing decision has no reason, because nothing refused"
        );
        assert_eq!(
            provider::Approval::Required("because".into()).why(),
            Some("because")
        );
        assert_ne!(APPROVAL_REQUIRED, OWNER);
        assert_ne!(APPROVAL_REQUIRED, "grant");
        assert_ne!(OWNER, "grant");
        assert_ne!(APPROVAL_REQUIRED, UNDECIDED);
        assert_ne!(OWNER, UNDECIDED);
    }

    // ── §13.13's reach: the audit filter a destroy plan is built from ────────

    #[test]
    fn the_audit_project_filter_is_exact_and_runs_before_the_line_limit() {
        // Both halves matter, and each is a way for §13.13's destroy plan to
        // be wrong.
        //
        // EXACT: a worktree lives at `<project>/.apex/worktrees/<name>`, so a
        // prefix match asking about the project would sweep in every
        // worktree's work — and a plan built from that would offer to delete
        // another worktree's preview. The other direction is worse: a plan for
        // the worktree that matched the project root as a prefix of nothing
        // would still be wrong when two worktrees share a name prefix.
        //
        // BEFORE THE LIMIT: the trail is one file for the machine. A client
        // that asked for the last thousand lines and filtered them itself
        // would see a thousand lines of somebody else's work and conclude this
        // worktree had created nothing — a plan that reports a clean worktree
        // because the machine is busy.
        let (svc, dir) = temp_service("audit-filter");
        let peer = me();
        svc.add(peer, demo_service("demo", "github.com", "https"), SecretValue::new(b"t".to_vec()));

        let here = "/tmp/p";
        let worktree = "/tmp/p/.apex/worktrees/one";
        // The two lines that matter go FIRST, and the noise after them. That
        // ordering is the whole of the second half: a build that took the last
        // `limit` lines and then filtered would find nothing here, because
        // everything it looked at happened afterwards. The other ordering
        // passes either way and would have proved nothing — measured, not
        // assumed.
        svc.use_capability(peer, record("demo", "git.fetch", "origin", here), Vec::new());
        svc.use_capability(peer, record("demo", "git.fetch", "origin", worktree), Vec::new());
        for i in 0..60 {
            let mut rec = record("demo", "git.fetch", "origin", "/tmp/elsewhere");
            rec.params.insert("branch".into(), format!("b{i}"));
            svc.use_capability(peer, rec, Vec::new());
        }

        let lines = |project: Option<&str>, limit: usize| -> Vec<AuditLine> {
            match svc.audit(peer, limit, project) {
                Response::Audit { entries } => entries,
                other => panic!("not an audit reply: {other:?}"),
            }
        };

        // A limit far below the noise still reaches this project's own line.
        let mine = lines(Some(here), 5);
        assert_eq!(mine.len(), 1, "{mine:#?}");
        assert_eq!(mine[0].project.as_deref(), Some(here));

        // And the worktree under it is NOT this project's.
        let theirs = lines(Some(worktree), 5);
        assert_eq!(theirs.len(), 1, "{theirs:#?}");
        assert_eq!(theirs[0].project.as_deref(), Some(worktree));

        // Unfiltered still sees everything, so the filter added a question
        // rather than narrowing the verb.
        assert!(lines(None, 1000).len() > 60);
        std::fs::remove_dir_all(&dir).ok();
    }
}
