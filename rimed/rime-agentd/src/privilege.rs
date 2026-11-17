//! Filing and deciding privilege requests, daemon side.
//!
//! The daemon's whole job here is to be the thing that cannot be talked out of
//! the truth. It answers three questions:
//!
//!   * *who is asking* — from [`crate::peer`], never from the request;
//!   * *is this in the vocabulary* — from [`rime_agent_core::request::Verb`],
//!     which cannot express an arbitrary command;
//!   * *has a human already allowed exactly this here* — from the grant store.
//!
//! It does not execute anything. `rime-agentd` is unprivileged, and keeping it
//! that way is the point: §2 says agent orchestration must not live inside the
//! privileged daemon. An approved request is run by `rime request approve`
//! under the approving human's own root, and the daemon only records that it
//! happened.

use std::sync::Arc;

use rime_agent_core::grant::GrantKind;
use rime_agent_core::origin::{OriginError, OriginSource, SessionOrigin};
use rime_agent_core::paths;
use rime_agent_core::policy::{AgentPolicy, OriginPolicy, RequestOrigin};
use rime_agent_core::protocol::{ErrorKind, Response, SubmittedFactor};
use rime_agent_core::webauthn::{
    self, AssertionError, Elevation, RemoteElevationRefused, SecondFactor,
};
use rime_agent_core::request::{
    self, Decision, Grants, PrivilegeRequest, RequestError, Verb,
};

use crate::grants::{authenticate, Authenticated, GrantError};
use crate::peer::{self, Peer};
use crate::Daemon;

/// The connection a request arrived on.
///
/// Two things, and the reason they travel together is that neither is safe on
/// its own. [`Caller::peer`] is what the kernel says — the pid, uid and gid
/// `SO_PEERCRED` filled in at `connect(2)`, which no client can forge.
/// [`Caller::declared`] is a narrowing the connection asked for and was
/// granted, which is a claim — but a claim that [`rime_agent_core::origin::may_declare`]
/// has already refused unless it gives something up, re-checked against the
/// live observation every time it is read.
///
/// It replaces the bare `Option<Peer>` that used to be threaded through every
/// handler, and that is the point of the type rather than a second parameter:
/// a handler cannot read the peer and forget the declaration, because there is
/// nothing to read but this.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    /// What the kernel says about the other end, or `None` when it would not
    /// say. `None` is unauthenticated, never trusted.
    pub peer: Option<Peer>,
    /// An origin this connection declared for itself, latched for the life of
    /// the connection.
    ///
    /// Only ever consulted for a connection that does not resolve to a managed
    /// session: a session's origin is the one recorded on the session, and a
    /// session narrows itself by narrowing that record.
    declared: Option<RequestOrigin>,
    /// Who the declaration was made for, from `DeclareOrigin`'s `actor`.
    actor: Option<String>,
}

impl Caller {
    /// A connection with nothing declared on it yet.
    pub fn new(peer: Option<Peer>) -> Caller {
        Caller {
            peer,
            declared: None,
            actor: None,
        }
    }

    /// Record an accepted declaration.
    ///
    /// Only [`declare`] calls this, and only after `may_declare` has passed.
    /// The value is stored rather than the resulting [`SessionOrigin`] so that
    /// [`origin`] re-derives it from the *live* observation on every request:
    /// a stored `SessionOrigin` would keep asserting a narrowing whose basis
    /// had gone, and re-deriving costs two `/proc` reads that were already
    /// happening.
    fn latch(&mut self, origin: RequestOrigin, actor: Option<String>) {
        self.declared = Some(origin);
        if actor.is_some() {
            self.actor = actor;
        }
    }
}

/// Which session a connection belongs to, what that session was doing, and
/// where it is being driven from.
///
/// `session` is `None` for a connection from an ordinary terminal — the user's
/// own shell running `rime request approve`, or Rime Shell. That is not an
/// error: an unsessioned peer is the human, and the human is who decides.
#[derive(Debug, Clone, Default)]
pub struct Origin {
    pub session: Option<u32>,
    pub agent: Option<String>,
    pub project: Option<String>,
    /// The permission dimensions the asking session runs under.
    ///
    /// Taken from what the daemon recorded when it forked the session, never
    /// from the request: a session that could name its own policy could name a
    /// looser one. An unsessioned peer is the human, whose policy is whatever
    /// their shell allows, so the default applies.
    pub policy: AgentPolicy,
    /// Where the connection came from (§7's `request_origin`), when that could
    /// be established.
    pub request_origin: Option<SessionOrigin>,
    /// Why [`Origin::request_origin`] is not a measurement, when it is not
    /// one.
    ///
    /// A separate field rather than a `Result`, so `Origin::default()` and the
    /// callers that only want `session` keep working, and so the reason
    /// survives to the refusal the user reads. `None` here with `None` above
    /// cannot happen from [`origin`]: one of the two is always set.
    pub origin_unreadable: Option<String>,
    /// Which remote actor this connection declared itself to be acting for.
    ///
    /// Only ever set from an accepted [`Request::DeclareOrigin`], so it exists
    /// exactly where `request_origin` is a declaration and is `None`
    /// everywhere else. A display string; never a key.
    pub actor: Option<String>,
}

impl Origin {
    /// An origin the daemon could not establish, with the reason.
    ///
    /// Deliberately not a fallback to [`RequestOrigin::default`], which is
    /// `local-terminal` — the origin §7 reserves root and break-glass for.
    /// "Could not tell" and "a human is at the keyboard" are different
    /// answers, and this is the whole defect this field exists to avoid.
    fn unreadable(why: impl Into<String>) -> Origin {
        Origin {
            origin_unreadable: Some(why.into()),
            ..Origin::default()
        }
    }

}

/// The origin of a connection that is not a managed session, with the
/// connection's own declaration applied over the observation.
///
/// The declaration is checked here, at the moment it is used, and not only
/// when it was accepted. Two reasons, and the second is the one that matters:
///
///  * the observation is re-read every request anyway, and a latch that
///    narrowed `scheduled-job` is a different claim once the peer's cgroup
///    says something else;
///  * `may_declare` is the whole security property, and a design where it runs
///    once and a stored value is trusted afterwards has moved the check away
///    from the thing it protects. Here, a narrowing that has stopped being a
///    narrowing simply stops applying, and the observation stands.
///
/// A declaration that is refused is not an error: the connection gets the
/// origin it actually has, which is never *less* restricted than what it
/// asked for. Refusing outright would let a client take a connection down by
/// asking for something it was already going to be denied.
fn connection_origin(observed: Result<RequestOrigin, String>, caller: &Caller) -> Origin {
    let observed = match observed {
        Ok(o) => o,
        Err(why) => return Origin::unreadable(why),
    };
    let base = SessionOrigin::observed(observed);
    let (request_origin, actor) = match caller.declared {
        Some(wanted) => match base.declare(wanted) {
            Ok(narrowed) => (narrowed, caller.actor.clone()),
            Err(_) => (base, None),
        },
        None => (base, None),
    };
    Origin {
        request_origin: Some(request_origin),
        actor,
        ..Origin::default()
    }
}

/// Resolve a connection to the session that owns it, and to §7's origin.
///
/// The pid comes from `SO_PEERCRED` and is walked up its `/proc` ancestry until
/// it meets a pid the daemon recorded when it forked a session. Nothing the
/// client sent is consulted.
///
/// A connection that belongs to a session **inherits that session's origin**
/// rather than being classified afresh. That is the point: a Remote Control
/// session's agent files a request from a local process, so re-observing it
/// would answer `local-terminal` every time and the second column of §7's
/// table would be unreachable. The session's origin is what is actually
/// driving it, and the daemon recorded it when it forked the session.
///
/// A connection that is **not** a session may have latched a narrower origin
/// onto itself with [`declare`], and that is applied here rather than at the
/// point it was accepted. Re-derived from the live observation on every
/// request, so the narrowing stands only while it is still a narrowing: a
/// latch is a claim, and the check that makes it safe is the one that runs
/// when it is used.
pub fn origin(daemon: &Arc<Daemon>, caller: &Caller) -> Origin {
    let Some(peer) = caller.peer else {
        return Origin::unreadable(
            "the kernel would not report the peer credentials of this connection, so there is \
             no way to tell where it came from",
        );
    };
    // Where a connection came from is a question about the process, not about
    // which account it runs as, so it is answered before the uid is looked at.
    //
    // That ordering matters for exactly one caller and it is the important
    // one: `rime request approve` runs under `sudo`, so it reaches this socket
    // as uid 0. It is still a human at a terminal, and §4's whole approval
    // path is that command. Classifying it from its cgroup — which is
    // world-readable and which sudo does not let a process choose — is what
    // keeps that true without widening anything: root can already read the
    // request files and run the operation directly.
    let observed = crate::origin::observe(&peer);

    if !peer::is_own_user(&peer) {
        // Not this user's process, so it is not one of this user's sessions —
        // the ancestry walk below would be attributing a foreign process to a
        // session it cannot be inside. The origin still stands.
        return connection_origin(observed, caller);
    }

    // Snapshot (pid, id) pairs, then release the lock: the ancestry walk does
    // filesystem I/O and must not hold the registry while it does.
    let live: Vec<(libc::pid_t, u32)> = {
        let reg = daemon.registry.lock().expect("registry lock");
        reg.list()
            .iter()
            .filter_map(|h| {
                let s = h.lock().ok()?;
                Some((s.pid, s.info.id))
            })
            .collect()
    };

    let matched = if live.is_empty() {
        None
    } else {
        peer::resolve_by_ancestry(peer.pid, |p| live.iter().any(|(pid, _)| *pid == p))
    };

    let Some(found) = matched.and_then(|f| live.iter().find(|(pid, _)| *pid == f).map(|(_, id)| *id))
    else {
        // Not a managed session, so the peer's own classification is the
        // answer — narrowed by whatever this connection has latched.
        return connection_origin(observed, caller);
    };

    // Re-take the lock for the details, so the snapshot above stays short.
    let reg = daemon.registry.lock().expect("registry lock");
    let found_session = reg.get(found).and_then(|h| {
        h.lock().ok().map(|s| {
            (
                s.info.agent.clone(),
                s.info.project.clone(),
                s.info.policy,
                s.info.request_origin,
            )
        })
    });
    match found_session {
        Some((agent, project, policy, recorded)) => Origin {
            session: Some(found),
            agent: Some(agent),
            project,
            policy,
            actor: None,
            request_origin: recorded.map(SessionOrigin::inherited),
            // A session record from a daemon that predates origin tracking.
            // Not treated as local: the field was never written, and an
            // absent field is not a measurement.
            origin_unreadable: recorded.is_none().then(|| {
                format!(
                    "session {found} was started before this runtime recorded origins, so where \
                     it is driven from was never established"
                )
            }),
        },
        None => Origin {
            session: Some(found),
            agent: None,
            project: None,
            // A session whose record vanished between the ancestry walk and
            // this lookup gets the default, which is the strict value for
            // every dimension. Failing toward the loose one here would make a
            // race into a permission.
            policy: AgentPolicy::default(),
            request_origin: None,
            actor: None,
            origin_unreadable: Some(format!(
                "session {found} disappeared while this request was being attributed"
            )),
        },
    }
}

/// Whether a connection may write text into session `target`'s terminal.
///
/// `None` means it may. A [`Response`] means it may not, and says why.
///
/// A function over an established [`Origin`] and nothing else, so the decision
/// is testable without a daemon, a socket or a session — which is the pattern
/// the rest of this file follows, and the reason `tests/request_origin.rs`
/// exists only for the two claims that genuinely need a spawned process.
///
/// `Request::Input` is the one verb on this socket that acts on a session
/// other than the caller's own AND is invisible to the target as anything but
/// a person at the keyboard. `Signal` reaches another session too, but an
/// agent sees a signal as a signal; bytes on a PTY are typing. So a session
/// that could send them could instruct a sibling agent and borrow whatever
/// that sibling was granted, which is dimension 3 leaking sideways.
///
/// Two refusals, not one, and the second is the one worth writing down:
///
///  * the caller IS a session. Refused.
///  * the caller could not be classified at all. Also refused. A connection
///    whose credentials the kernel would not report is not evidence of a
///    human; treating "could not read" as "not a session" is exactly the
///    defect [`Origin::unreadable`] exists to prevent, one verb further on.
///    Fail closed: the cost is a `rime agent input` that reports it could not
///    establish the caller, against an agent quietly driving another agent.
pub fn refuse_input(who: &Origin, target: u32) -> Option<Response> {
    if let Some(caller) = who.session {
        return Some(Response::error(
            ErrorKind::PermissionDenied,
            format!(
                "session {caller} may not write into session {target}'s terminal; text on a \
                 terminal cannot be told apart from a person typing"
            ),
        ));
    }
    if let Some(why) = who.origin_unreadable.as_deref() {
        return Some(Response::error(
            ErrorKind::PermissionDenied,
            format!(
                "refusing to write into session {target}: this connection could not be \
                 classified, so there is no way to tell it is not a session driving another \
                 session ({why})"
            ),
        ));
    }
    None
}

/// The predicate [`refuse_input`] applies, with the sentences left to the verb.
///
/// `refuse_input` and `refuse_clipboard` each spell the rule out, and a test
/// holds them to the same answer on every origin. `rename` and `peek` are two
/// more verbs under exactly that rule (`docs/remote-live-contract.md` §1.5,
/// §1.6), and a fourth copy of the same two `if`s would be one careless edit
/// from a gate that answers a different question — so they share this.
fn refuse_unless_a_person(
    who: &Origin,
    as_session: impl FnOnce(u32) -> String,
    unclassified: impl FnOnce(&str) -> String,
) -> Option<Response> {
    if let Some(caller) = who.session {
        return Some(Response::error(ErrorKind::PermissionDenied, as_session(caller)));
    }
    if let Some(why) = who.origin_unreadable.as_deref() {
        return Some(Response::error(ErrorKind::PermissionDenied, unclassified(why)));
    }
    None
}

/// Whether a connection may name session `target` (§1.5).
///
/// [`refuse_input`]'s predicate. Only a person names a session: the name is
/// what the Agent Center, `rime agent list` and a phone show a person, so an
/// agent that could rename a sibling could make one session read as another
/// on the screen somebody approves things from.
pub fn refuse_rename(who: &Origin, target: u32) -> Option<Response> {
    refuse_unless_a_person(
        who,
        |caller| {
            format!(
                "session {caller} may not rename session {target}; a session's name is what a \
                 person reads to tell sessions apart, and only a person sets it"
            )
        },
        |why| {
            format!(
                "refusing to rename session {target}: this connection could not be classified, \
                 so there is no way to tell it is not a session ({why})"
            )
        },
    )
}

/// Whether a connection may read the tail of session `target`'s screen
/// (§1.6).
///
/// [`refuse_input`]'s predicate, for the opposite direction's reason: agents
/// do not read each other's screens. What is on one is whatever that agent
/// was shown — a diff, a key it printed, a question it is answering.
pub fn refuse_peek(who: &Origin, target: u32) -> Option<Response> {
    refuse_unless_a_person(
        who,
        |caller| {
            format!(
                "session {caller} may not read session {target}'s screen; one agent does not \
                 read what another was shown"
            )
        },
        |why| {
            format!(
                "refusing to show session {target}'s screen: this connection could not be \
                 classified, so there is no way to tell it is not a session ({why})"
            )
        },
    )
}

/// Which kind of viewer a connection is, for whose size a session's terminal
/// takes (§1.7).
///
/// A phone is exactly a connection whose origin is `claude-remote-control`:
/// `rime-remoted` declares that on every connection it opens, before anything
/// else, and refuses to forward a byte if the declaration is not accepted —
/// so a phone's attach AND its resize, which arrive on two different
/// connections, carry the same class. Attributed by origin and never by
/// socket, because the socket is the one thing the two do not share.
///
/// Everything else is local, including the origins §7 does not call local —
/// a scheduled job, an unreadable one. That is deliberate and it is not a
/// permission question: nothing here grants anything, it decides whose
/// terminal size to give back. The failure it avoids is concrete. A terminal
/// launched as a systemd app scope (uwsm does this for every app) sits under
/// `user@N.service` and classifies as `scheduled-job`, and treating that as
/// "not local" would make the desktop's own terminal the one whose size is
/// never remembered — the bug this exists to fix, on those machines.
pub fn viewer(who: &Origin) -> rime_agent_core::session::Viewer {
    use rime_agent_core::session::Viewer;
    match who.request_origin {
        Some(o) if o.origin == RequestOrigin::RemoteControl => Viewer::Remote,
        _ => Viewer::Local,
    }
}

/// Refuse `clipboard_write` and `save_file` (contract §1.8) to the callers
/// [`refuse_clipboard`] refuses, for the reverse reason: a session that could
/// set the clipboard could change what a person pastes into a terminal a
/// moment later, and one that could drop files in Downloads could leave
/// something there for a person to open. `doing` finishes "may not …".
pub fn refuse_desktop(who: &Origin, doing: &str) -> Option<Response> {
    if let Some(caller) = who.session {
        return Some(Response::error(
            ErrorKind::PermissionDenied,
            format!(
                "session {caller} may not {doing}; only a person at this machine or on a phone \
                 paired with it may"
            ),
        ));
    }
    if let Some(why) = who.origin_unreadable.as_deref() {
        return Some(Response::error(
            ErrorKind::PermissionDenied,
            format!(
                "refusing to {doing}: this connection could not be classified, so there is no \
                 way to tell it is not a session asking ({why})"
            ),
        ));
    }
    None
}

/// Whether a connection may read what is on the machine's clipboard.
///
/// `None` means it may. A [`Response`] means it may not, and says why.
///
/// ## The same predicate as [`refuse_input`], and it is not laziness
///
/// Two refusals, and they are [`refuse_input`]'s two: a caller that IS a
/// managed session, and a caller this daemon could not classify at all. The
/// sentences differ because the verb differs, and the sentence is what the
/// person reads — but the rule is the same rule, and writing it as a second
/// predicate that happened to agree today is how two gates drift into
/// answering different questions.
///
/// What makes a session the caller to refuse here is not the same *reason* it
/// is refused from `input`, though. `input` is refused because bytes on a PTY
/// cannot be told from a person typing, so a session could drive a sibling.
/// This is refused because of what a clipboard HOLDS. A person copies a
/// password out of a password manager and pastes it thirty seconds later; an
/// agent that could ask this verb would have a window on every one of those,
/// outside every sandbox, with nothing on screen and no grant spent. There is
/// no session-scoped version of that to offer instead — one seat has one
/// clipboard — so the answer is no, and the answer does not depend on which
/// session is asking.
///
/// A caller that could not be classified is refused for the reason it is
/// everywhere else in this file: "could not read" is not evidence of a human,
/// and treating it as one is the defect [`Origin::unreadable`] exists to
/// prevent. Fail closed. The cost is a phone told the machine could not work
/// out who was asking, against an agent quietly reading a password.
///
/// **Not** `inject`'s gate (`inject::handle`), and the difference is the whole
/// design. `inject` additionally refuses a non-local origin, because its
/// `source` is a host path read with the daemon's own access. A clipboard read
/// names no path and reaches nothing a person did not put on a clipboard
/// themselves, and refusing a remote origin would refuse the only caller the
/// verb exists for — the same reasoning `Request::Receive` records.
pub fn refuse_clipboard(who: &Origin) -> Option<Response> {
    if let Some(caller) = who.session {
        return Some(Response::error(
            ErrorKind::PermissionDenied,
            format!(
                "session {caller} may not read this machine's clipboard; a clipboard holds \
                 whatever a person last copied, which is regularly a password, and nothing \
                 about that is scoped to a session"
            ),
        ));
    }
    if let Some(why) = who.origin_unreadable.as_deref() {
        return Some(Response::error(
            ErrorKind::PermissionDenied,
            format!(
                "refusing to read this machine's clipboard: this connection could not be \
                 classified, so there is no way to tell it is not a session asking ({why})"
            ),
        ));
    }
    None
}

/// Record a narrower origin (§7), for a session or for a connection.
///
/// Two callers, one rule. A **session** narrows its own record, permanently:
/// the session comes from the peer credentials, so there is no id to get wrong
/// or to forge, and every later request the session makes inherits it. That is
/// the route Remote Control takes — it is switched on after `claude` has
/// started, so nothing observable about the connection ever changes and an
/// observation alone could never reach §7's second column.
///
/// A **connection that is not a session** narrows itself, for as long as it is
/// open. That is the route a remote proxy takes, and it is the case the daemon
/// had no answer for: `rime-remoted` is not a session and never will be, so
/// every request it forwarded for a paired phone would carry the origin its
/// own cgroup implies. Started as a user unit that is `scheduled-job`, which
/// is at least not local; started from a login session — a developer running
/// it in a terminal, a shell launching it as a child — it is `local-terminal`,
/// and `decide` accepts that as a human at the keyboard. A remote request
/// approving root is precisely what §7 forbids, and nothing in the daemon
/// stood between the two.
///
/// Both routes go through [`rime_agent_core::origin::may_declare`], which
/// accepts a declaration only when it gives something up, and neither can
/// produce a local origin: [`RequestOrigin::may_be_declared`] refuses those by
/// name, whatever was observed.
///
/// `caller` is `&mut` for the connection case, which is the only mutation:
/// the latch lives on the connection, not in any store, so it dies with the
/// socket and cannot be inherited by anything else.
pub fn declare(
    daemon: &Arc<Daemon>,
    caller: &mut Caller,
    wanted: &str,
    actor: Option<String>,
) -> Response {
    let Some(wanted) = RequestOrigin::parse(wanted) else {
        return Response::error(
            ErrorKind::BadRequest,
            format!(
                "'{wanted}' is not an origin; use one of {}",
                RequestOrigin::ALL
                    .iter()
                    .filter(|o| o.may_be_declared())
                    .map(|o| o.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    };

    // Two refusals that are functions of the REQUEST and of nothing else, and
    // they are answered here — before the daemon works out who is calling —
    // so that they read the same way in every environment.
    //
    // A reordering, made after CI showed what leaving it costs.
    // `may_be_declared` depends only on `wanted` and `unprintable_actor` only
    // on `actor`, but both used to sit behind the observation of the caller's
    // own origin. That observation fails outright where the cgroup is neither
    // a login session nor a user service — a GitHub runner sits in
    // `/system.slice/hosted-compute-agent.service` — so a caller that asked
    // to declare `local-terminal` was told the daemon could not read /proc.
    // True, and not the thing they got wrong; and a test asserting the real
    // rule could not pass there.
    //
    // This refuses strictly MORE, never less. Nothing that reached the latch
    // before reaches it now: the observation check below still stands, and
    // these two only move earlier. The gate that says a runner has no human
    // at it is untouched, because it is not this one.
    if !wanted.may_be_declared() {
        return Response::error(
            ErrorKind::PermissionDenied,
            OriginError::NotDeclarable(wanted).to_string(),
        );
    }
    if let Some(bad) = actor.as_deref().and_then(unprintable_actor) {
        return Response::error(ErrorKind::BadRequest, bad);
    }

    let who = origin(daemon, caller);
    let Some(session) = who.session else {
        return declare_on_connection(who, caller, wanted, actor);
    };
    let Some(current) = who.request_origin else {
        return Response::error(
            ErrorKind::PermissionDenied,
            who.origin_unreadable
                .unwrap_or_else(|| "this session has no recorded origin".to_string()),
        );
    };
    let narrowed = match current.declare(wanted) {
        Ok(o) => o,
        Err(e) => return Response::error(ErrorKind::PermissionDenied, e.to_string()),
    };

    let reg = daemon.registry.lock().expect("registry lock");
    let Some(handle) = reg.get(session) else {
        return Response::error(ErrorKind::NoSuchSession, format!("no session {session}"));
    };
    let mut s = handle.lock().expect("session lock");
    s.info.request_origin = Some(narrowed.origin);
    s.info.origin_source = Some(narrowed.source);
    crate::registry::write_record(&s.info);
    Response::Session(Box::new(s.info.clone()))
}

/// Latch a narrower origin onto a connection that is not a session.
///
/// Answers [`Response::Ok`] rather than a record, because there is no record:
/// the latch is connection state and dies with the socket. `Ok` is still the
/// answer a proxy needs, and it needs it to be load-bearing — `rime-remoted`
/// declares before it forwards anything and refuses to serve the device if
/// this comes back an error, because a proxy whose declaration was dropped is
/// a proxy forwarding remote requests under a local origin.
///
/// An unreadable observation is refused, not ignored. The latch would be inert
/// — [`unsessioned`] never applies one over an origin it could not establish —
/// and a silently inert narrowing is the failure mode this whole module is
/// written against.
fn declare_on_connection(
    who: Origin,
    caller: &mut Caller,
    wanted: RequestOrigin,
    actor: Option<String>,
) -> Response {
    let Some(current) = who.request_origin else {
        return Response::error(
            ErrorKind::PermissionDenied,
            who.origin_unreadable.unwrap_or_else(|| {
                "the origin of this connection could not be established, so there is nothing to \
                 narrow"
                    .to_string()
            }),
        );
    };
    if let Err(e) = current.declare(wanted) {
        return Response::error(ErrorKind::PermissionDenied, e.to_string());
    }
    // The actor was shape-checked in `declare`, before the observation above,
    // and this is its only caller — so a second copy here would be a check
    // whose failing branch nothing can reach, which is the pattern this unit
    // has now found six times. Moved rather than duplicated.
    caller.latch(wanted, actor);
    Response::Ok
}

/// The longest actor id the daemon will record.
///
/// Bounded because it is written into records the user reads and into the
/// audit log, and an unbounded string from a client is a line of that log a
/// client chose the length of.
const MAX_ACTOR: usize = 64;

/// Why an actor id is not fit to be recorded, or `None` when it is.
///
/// Refused rather than sanitised. The value ends up on a privilege prompt a
/// human reads before handing out root, and a control character there can
/// rewrite the line around it — an actor called `phone\r\nAPPROVED` is a
/// display attack, not a naming choice. Silently stripping it would leave the
/// caller believing they named a device that the record calls something else.
fn unprintable_actor(actor: &str) -> Option<String> {
    if actor.is_empty() {
        return Some("an actor was named but it is empty; omit the field instead".to_string());
    }
    if actor.chars().count() > MAX_ACTOR {
        return Some(format!(
            "an actor id may be at most {MAX_ACTOR} characters; this one is {}",
            actor.chars().count()
        ));
    }
    if actor.chars().any(|c| c.is_control()) {
        return Some(
            "an actor id may not contain control characters: it is printed on the prompt a human \
             reads before approving a root operation"
                .to_string(),
        );
    }
    None
}

/// The origin a new session gets, and the error when it cannot have one.
///
/// Split out of `session::start` so the rule is one readable function: a Run
/// from inside a session produces a child of that session's origin, a Run from
/// anywhere else is classified from the connection, and a declaration is
/// applied on top of whichever it was.
pub fn for_new_session(
    who: &Origin,
    declared: Option<rime_agent_core::policy::RequestOrigin>,
) -> Result<SessionOrigin, String> {
    let base = match (&who.request_origin, who.session) {
        (Some(parent), Some(_)) => SessionOrigin {
            origin: rime_agent_core::origin::child_of(parent.origin),
            source: OriginSource::Inherited,
        },
        (Some(observed), None) => *observed,
        (None, _) => {
            return Err(who
                .origin_unreadable
                .clone()
                .unwrap_or_else(|| "the origin of this connection could not be established".into()))
        }
    };
    match declared {
        None => Ok(base),
        Some(wanted) => base.declare(wanted).map_err(|e| e.to_string()),
    }
}

/// The one gate every system-access grant passes through, in the one order
/// that makes it mean anything (§3.3, §4.4, §7).
///
/// Four steps, and the first three are refusals that happen **before** polkit
/// is asked anything. That ordering is not tidiness: it is what makes the
/// checks testable against a real daemon with no risk of a password dialog,
/// and it is what puts the eventual prompt outside the agent's terminal.
///
///   1. **Not from inside a session.** This is P0-007's fourth criterion —
///      "the agent cannot renew its own grant" — and it is the only form that
///      criterion can take. The agent runs as the user, so "is this the user?"
///      cannot distinguish it from the human; every uid, every group, every
///      environment variable and every field in the request says the same
///      thing for both. What differs is the *connection*: the peer pid comes
///      from `SO_PEERCRED`, which the kernel fills in at `connect(2)`, and
///      `peer::resolve_by_ancestry` walks it up `/proc` to the pid the daemon
///      recorded when it forked the session. Every process inside the sandbox
///      is a descendant of that pid, and a process cannot choose its parent.
///
///      The escape a reader will think of — reparenting to pid 1 by orphaning
///      — does not work either, and not by luck: an orphan of a session is
///      reparented inside the user manager, so its cgroup is
///      `…/user@N.service/…`, and [`crate::origin::classify`] reads that as
///      `scheduled-job`, which fails step 2. The two mechanisms cover each
///      other, which is why both are here.
///
///   2. **Local origin.** §7's table gives root capability and
///      unsafe-everything "local auth" locally and "local approval required"
///      from everywhere else. A `scheduled-job`, an `mcp` server and a
///      `subagent` are all non-local by [`RequestOrigin::is_local`], because
///      the property that matters is whether a human is present.
///
///   3. **An origin at all.** An origin that could not be established is
///      refused, never defaulted — `RequestOrigin::default()` is
///      `local-terminal`, which is precisely the column being asked for.
///
///   4. **polkit**, with the peer as the subject.
pub fn may_be_granted(who: &Origin, what: &'static str) -> Result<RequestOrigin, GrantError> {
    if let Some(session) = who.session {
        return Err(GrantError::FromInsideASession { session, what });
    }
    let Some(source) = who.request_origin else {
        return Err(GrantError::OriginUnknown(
            who.origin_unreadable
                .clone()
                .unwrap_or_else(|| "the origin of this connection could not be established".into()),
        ));
    };
    if !source.is_local() {
        return Err(GrantError::NotLocal {
            origin: source.origin,
            what,
        });
    }
    Ok(source.origin)
}

/// What is being elevated, for the gate §7's second column goes through
/// (P0-014).
///
/// A struct rather than three more parameters on [`authorise_grant`] — and the
/// policy is **passed rather than looked up**, which is the part of this a
/// reader will want to argue with. Two reasons, both measured before a line of
/// it was written:
///
///   - **The two callers find it in different places.** A session being started
///     is governed by the request's own `--origin-policy`; a renewal is
///     governed by the recorded policy of the session that holds the grant.
///     Looking it up in here would have to pick one and be wrong for the other.
///   - **`Config.origin` is not it.** `Config::policy()` assembles the
///     *default* `AgentPolicy` for sessions started without the flags, and a
///     session started with `--origin-policy remote` overrides it. Reading the
///     config at the gate would mean the per-session dimension P0-014 is named
///     after governs nothing — and a naive test would pass.
///
/// The same reasoning rules out `who.policy.origin`, which is what a reader
/// reaching for the nearest field would use: [`may_be_granted`] returns early
/// for a sessioned connection, `origin()` assigns `policy` only on its two
/// sessioned branches, and every unsessioned branch is `..Origin::default()`.
/// So `who.policy.origin` at this point is provably always
/// `LocalElevationOnly` — a gate that could never open.
pub struct Elevating<'a> {
    /// The governing per-session policy. See above for why it is an argument.
    pub policy: OriginPolicy,
    /// The session being elevated, or `None` for one being started.
    ///
    /// `None` is the *primary* case, and not an oversight:
    /// [`authorise_grant`] runs before the session id is reserved, so that a
    /// refused password leaves nothing behind. `SecondFactor::may_answer_for`
    /// compares this in both directions, so a touch collected while starting a
    /// session cannot renew an existing grant, and a touch for session 7
    /// cannot start a new one.
    pub scope: Option<u32>,
    /// The window being asked for, already bounded by `grant::ttl_for`. Part
    /// of what the key signed.
    pub ttl_ms: u64,
    /// What the client sent back, if a key answered at all.
    pub factor: Option<&'a SubmittedFactor>,
}

/// Which of §7's two columns approved an elevation, and the proof if it was
/// the second.
///
/// The two columns are authenticated by different things, and the difference
/// has to survive out of [`decide_origin`] rather than being re-derived from
/// the origin afterwards: re-deriving it would mean writing `is_local()` twice
/// and trusting the two copies to agree. [`Approved::ByKey`] carries the
/// receipt, so the branch that skips polkit cannot be reached without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approved {
    /// A local origin. polkit still decides, exactly as it did before P0-014.
    Local(RequestOrigin),
    /// A non-local origin, the owner's policy permitting it, and a verified
    /// assertion for this exact elevation. polkit is **not** asked — see
    /// [`crate::grants::Authenticated`] for why asking it would be a wall
    /// rather than a second lock.
    ByKey(RequestOrigin, SecondFactor),
}

/// §7's two columns, decided over values.
///
/// Split out of [`authorise_grant`] so the decision is testable without a
/// daemon: everything here is a function of the origin, the policy and what
/// answered. Redeeming the nonce is the one part that needs the daemon, so it
/// arrives as a closure — which also makes "the nonce is not spent for a
/// caller that was refused before the gate" an assertion a test can make, by
/// passing a closure that panics.
///
/// The order is the security property:
///
///   1. **[`may_be_granted`]'s first, third and fourth steps still decide, and
///      still first.** A connection from inside a session, and an origin that
///      could not be established, are refused here — ahead of any policy and
///      any key. An agent cannot buy itself root with a touch, and a
///      connection nobody could identify is not offered the chance to try.
///   2. **Its second step — the local column — is the one §7 lets the owner
///      open**, so a non-local origin is carried forward to the gate instead
///      of being returned as [`GrantError::NotLocal`].
///   3. **[`webauthn::may_elevate`] then decides once, for both columns.** It
///      declines to have an opinion about a local origin, which is why this is
///      not a second, weaker copy of step 1: the local column is
///      `may_be_granted` plus polkit, and it stays that way.
///   4. **Only if the gate said "nothing answered" *and* something did answer,
///      badly, is the assertion's own error surfaced.** That is the reason
///      `may_elevate` checks the policy before the factor: under a policy the
///      owner never opted out of, the sentence the user reads has to name the
///      setting they must change, not the key they do not have.
fn decide_origin(
    who: &Origin,
    what: &'static str,
    kind: GrantKind,
    elevating: &Elevating<'_>,
    redeem: impl FnOnce(&SubmittedFactor) -> Result<SecondFactor, AssertionError>,
) -> Result<Approved, GrantError> {
    let origin = match may_be_granted(who, what) {
        Ok(local) => local,
        Err(GrantError::NotLocal { origin, .. }) => origin,
        Err(other) => return Err(other),
    };
    // Spent only once the origin is settled, and never for a caller step 1
    // already refused: a session that sends a touch has its request refused
    // without also having its challenge burned. A local caller that sends one
    // does spend it for nothing, which is harmless and is not worth a second
    // copy of `is_local()` here to avoid.
    let answered = elevating.factor.map(redeem);
    let elevation = Elevation {
        scope: elevating.scope,
        kind,
        ttl_ms: elevating.ttl_ms,
        origin,
    };
    let receipt = answered.as_ref().and_then(|a| a.as_ref().ok());
    match webauthn::may_elevate(elevating.policy, &elevation, receipt) {
        // §7's first column. Unchanged by P0-014, and deliberately so: the
        // local path is `may_be_granted` plus polkit, and a key that happened
        // to answer does not replace the password there.
        Ok(()) if origin.is_local() => Ok(Approved::Local(origin)),
        // The second column. `may_elevate` lets a non-local origin through
        // only when a receipt answered for this exact elevation — every other
        // way out of it is an `Err` — so the receipt is here by that
        // function's contract.
        Ok(()) => match receipt.cloned() {
            Some(verified) => Ok(Approved::ByKey(origin, verified)),
            // Unreachable through `may_elevate` as written. Left as a refusal
            // rather than an `expect`, because the single thing that must
            // never happen on this path is a non-local caller being approved
            // with no proof — and a future edit to `may_elevate` that made
            // this reachable would otherwise turn a panic into exactly that.
            None => Err(GrantError::RemoteElevation(
                RemoteElevationRefused::NoSecondFactor { origin },
            )),
        },
        Err(RemoteElevationRefused::NoSecondFactor { origin }) => match answered {
            // A key answered and the answer did not check out. The gate can
            // only say "nothing answered", because a refused assertion never
            // became a receipt; why it was refused is here.
            Some(Err(why)) => Err(GrantError::SecondFactorRefused(why)),
            _ => Err(GrantError::RemoteElevation(
                RemoteElevationRefused::NoSecondFactor { origin },
            )),
        },
        Err(refused) => Err(GrantError::RemoteElevation(refused)),
    }
}

/// [`decide_origin`], then the password.
///
/// Returns the origin to record on the grant alongside the proof that a human
/// was asked. The proof is a token only this function and its sibling can
/// produce, so `GrantAuthority::issue` cannot be reached without one.
///
/// P0-014 added the `elevating` argument and nothing else about the shape of
/// this function: the origin decision moved into [`decide_origin`], polkit
/// still comes last, and every refusal that used to happen before a password
/// dialog still does.
pub fn authorise_grant(
    daemon: &Arc<Daemon>,
    who: &Origin,
    caller: &Caller,
    kind: GrantKind,
    what: &'static str,
    elevating: &Elevating<'_>,
) -> Result<(RequestOrigin, Authenticated), GrantError> {
    authorise_grant_with(daemon, who, caller, kind, what, elevating, |sent| {
        crate::elevation::redeem(daemon, sent)
    })
}

/// [`authorise_grant`] with the redeem supplied, following the seam
/// [`decide_origin`] already uses for the same reason.
///
/// It exists for one assertion that cannot be made any other way: that a
/// non-local caller carrying a verified receipt is authorised **without
/// polkit being asked at all**. That is a statement about a call that does
/// *not* happen, so it can only be tested by watching an authenticator that
/// counts — and the real redeem would need a credential store on disk and a
/// challenge in the daemon's memory to get as far as the branch being tested.
/// The redeem wiring itself is covered by `tests/elevation_challenge.rs`.
#[allow(clippy::too_many_arguments)]
fn authorise_grant_with(
    daemon: &Arc<Daemon>,
    who: &Origin,
    caller: &Caller,
    kind: GrantKind,
    what: &'static str,
    elevating: &Elevating<'_>,
    redeem: impl FnOnce(&SubmittedFactor) -> Result<SecondFactor, AssertionError>,
) -> Result<(RequestOrigin, Authenticated), GrantError> {
    let approved = decide_origin(who, what, kind, elevating, redeem)?;
    match approved {
        // The key IS the authentication here, and this is the line that makes
        // P0-014's gate reachable in production rather than only in a test.
        //
        // polkit is deliberately not consulted. `org.rimeos.agent.policy`
        // gives both actions `allow_any: no` and `allow_active: auth_admin`,
        // and says in its own comment that "there is no password that makes a
        // remote caller local" — so a remote peer is refused under
        // `allow_any`, and an owner who also happened to be logged in locally
        // would get the dialog on the machine's desktop, which the remote
        // human cannot reach. Asking anyway would mean a perfect touch on an
        // enrolled key is always followed by a check hard-coded to say no:
        // the gate would pass every unit test and be dead on the machine.
        //
        // The XML is correct and is not changed. What changed is that
        // rime-agentd no longer asks polkit about a caller polkit has already
        // said it has no answer for.
        Approved::ByKey(origin, verified) => {
            Ok((origin, crate::grants::authenticated_by_key(&verified)))
        }
        // The local column, byte for byte what it was before P0-014.
        Approved::Local(origin) => {
            let Some(peer) = caller.peer else {
                return Err(GrantError::OriginUnknown(
                    "the kernel would not report the peer credentials of this connection".into(),
                ));
            };
            let proof = authenticate(daemon.auth.as_ref(), kind, &peer)?;
            Ok((origin, proof))
        }
    }
}

/// Every system-access grant, with the state each is in now.
///
/// Readable from anywhere, including from inside a session: an agent being
/// able to see that it holds a grant and when it runs out is the opposite of
/// a risk, and §3.4's "revocation control always visible" wants the listing
/// available wherever somebody is looking.
pub fn system_grants(daemon: &Arc<Daemon>) -> Response {
    let now = request::now_ms();
    let listed = daemon.grants.list(now);
    Response::SystemGrants {
        grants: listed.iter().map(|(g, _, _)| g.clone()).collect(),
        states: listed
            .iter()
            .map(|(_, state, said)| (state.as_str().to_string(), said.clone()))
            .collect(),
    }
}

/// Take a grant back before its window runs out.
///
/// §3.4: "revocation control always visible". No authentication — giving up
/// privilege is free, the same rule [`rime_agent_core::auth::required_for`]
/// states over values and the same one P0-016's toggle follows. It still
/// refuses a connection from inside a session, because a session revoking
/// ANOTHER session's grant is a session changing somebody else's permissions.
pub fn revoke_system_grant(daemon: &Arc<Daemon>, caller: &Caller, id: u32) -> Response {
    let who = origin(daemon, caller);
    if let Some(session) = who.session {
        return Response::error(
            ErrorKind::PermissionDenied,
            GrantError::FromInsideASession {
                session,
                what: "revoke a system-access grant",
            }
            .to_string(),
        );
    }
    match daemon.grants.revoke(id, request::now_ms()) {
        Ok(g) => Response::SystemGrants {
            states: vec![(
                g.state_at(request::now_ms(), daemon.grants.boot())
                    .as_str()
                    .to_string(),
                g.describe(request::now_ms(), daemon.grants.boot()),
            )],
            grants: vec![g],
        },
        Err(e @ GrantError::NoSuchGrant(_)) => {
            Response::error(ErrorKind::NoSuchRequest, e.to_string())
        }
        Err(e) => Response::error(ErrorKind::BadRequest, e.to_string()),
    }
}

/// Extend a grant that is still in force.
///
/// The verb P0-007's fourth criterion is about. It goes through
/// [`authorise_grant`], so the agent holding the grant is refused at step 1 —
/// before polkit is asked and regardless of what it sends — and a fresh
/// password is required of everybody else. A renewal is a new window on a new
/// consent, not a longer one on the old.
pub fn renew_system_grant(
    daemon: &Arc<Daemon>,
    caller: &Caller,
    id: u32,
    ttl_ms: u64,
    second_factor: Option<&SubmittedFactor>,
) -> Response {
    let who = origin(daemon, caller);
    // Who is asking, before what they are asking about. Two reasons, and the
    // first is the criterion: a session must be told that renewing is not a
    // session's business, not that the grant it named has gone — a refusal
    // that depended on which id was passed would be one an agent could probe
    // its way around. The second is that neither check needs polkit, so the
    // ordering costs nothing.
    //
    // P0-014 narrowed this to everything EXCEPT `NotLocal`, and that narrowing
    // is the whole of §7's second column on this verb. A non-local caller
    // refused here would be refused *before* the gate, which is a refusal the
    // owner's own `OriginPolicy` never gets to see — the enforcement would
    // have been unreachable no matter which key answered. Every other refusal
    // still happens here: from inside a session, and an origin that could not
    // be established, both still ahead of polkit and ahead of the lookup.
    match may_be_granted(&who, "renew a system-access grant") {
        Ok(_) | Err(GrantError::NotLocal { .. }) => {}
        Err(e) => return Response::error(ErrorKind::PermissionDenied, e.to_string()),
    }
    // Then: the grant has to exist and be alive before anybody is asked for a
    // password. Prompting for a grant that has already expired teaches people
    // to type their password at dialogs that achieve nothing.
    let Some((kind, session)) = daemon
        .grants
        .list(request::now_ms())
        .into_iter()
        .find(|(g, state, _)| g.id == id && state.is_active())
        .map(|(g, _, _)| (g.kind, g.session))
    else {
        return Response::error(
            ErrorKind::NoSuchRequest,
            format!(
                "no active system-access grant {id}; a grant is issued to a session when it \
                 starts, so start a new session with the mode you need"
            ),
        );
    };
    // The window, checked before the gate rather than inside
    // `GrantAuthority::renew` after it. `grant::ttl_for` validates and does not
    // clamp, so the number a key signs over is the number that arrived either
    // way — what moving it earlier buys is that a typo in `--ttl` is refused
    // in front of the user instead of after a password dialog, and, since
    // P0-014, instead of after a challenge has been spent on it. `renew`
    // re-checks; this is not the enforcement.
    if let Err(e) = rime_agent_core::grant::ttl_for(kind, Some(ttl_ms)) {
        return Response::error(ErrorKind::BadRequest, GrantError::Ttl(e).to_string());
    }
    // §7's governing policy for this verb is the recorded policy of the
    // session that HOLDS the grant — see `Elevating` for why it is passed
    // rather than looked up in there, and why neither the config's default nor
    // the asking connection's own policy is it. The asking connection is the
    // human's shell, whose policy is the default.
    //
    // Read in its own block so the registry lock is released before
    // `authorise_grant` takes the challenge lock, following the convention
    // `origin` above already sets. Fails closed: a grant whose session the
    // registry no longer knows is governed by `OriginPolicy::default()`, which
    // is `LocalElevationOnly`.
    let policy = {
        let reg = daemon.registry.lock().expect("registry lock");
        reg.get(session)
            .and_then(|h| h.lock().ok().map(|s| s.info.policy.origin))
            .unwrap_or_default()
    };
    let elevating = Elevating {
        policy,
        // The grant names its session, so unlike a session being started this
        // caller has an id — and a touch collected for a session start cannot
        // be spent here, because `may_answer_for` compares the two directions.
        scope: Some(session),
        ttl_ms,
        factor: second_factor,
    };
    let proof = match authorise_grant(
        daemon,
        &who,
        caller,
        kind,
        "renew a system-access grant",
        &elevating,
    ) {
        Ok((_, proof)) => proof,
        Err(e) => return Response::error(ErrorKind::PermissionDenied, e.to_string()),
    };
    match daemon.grants.renew(proof, id, ttl_ms, request::now_ms()) {
        Ok(g) => Response::SystemGrants {
            states: vec![(
                g.state_at(request::now_ms(), daemon.grants.boot())
                    .as_str()
                    .to_string(),
                g.describe(request::now_ms(), daemon.grants.boot()),
            )],
            grants: vec![g],
        },
        Err(e) => Response::error(ErrorKind::BadRequest, e.to_string()),
    }
}


/// Which authority decided a request, and how the record must say so.
///
/// Two things can decide a request before a human sees it, and they are not
/// the same decision:
///
/// * a standing per-project grant (P0-013) — a human said yes to this
///   operation in this project, once, and meant it to keep holding. The
///   record says `allow_for_project`, which is what the next identical
///   request in that project will find.
/// * a session-scoped system-access grant (§4.4) — a human authenticated at
///   the top of *this* session for a stated window. It decided this request
///   and nothing beyond it: the window can be revoked or run out before the
///   next one. The record says `allow_once` and names the grant, so this
///   trail can be joined to the `RIME_GRANT_ID` journald holds for the same
///   window.
///
/// Recording a session grant as `allow_for_project` would put a standing
/// grant in the trail that no human ever made and that nothing on disk
/// backs — the audit would be describing an authority that does not exist.
///
/// A project grant wins when both apply, and the request is *not* attributed
/// to the session grant: it would have been allowed with no window open.
fn decided_by(
    by_project: bool,
    by_grant: Option<u32>,
) -> (Option<Decision>, Option<u32>, &'static str) {
    match (by_project, by_grant) {
        (true, _) => (Some(Decision::AllowForProject), None, "requested-and-granted"),
        (false, Some(id)) => (Some(Decision::AllowOnce), Some(id), "requested-and-covered"),
        (false, None) => (None, None, "requested"),
    }
}

/// File a request.
///
/// The verb is parsed here rather than accepted pre-parsed, so a client cannot
/// hand over a `Verb` that never went through validation. On a project where a
/// human has already allowed exactly this, the request is recorded as decided
/// and needs no prompt — see [`Decision::AllowForProject`] for what that does
/// and does not buy.
pub fn file(daemon: &Arc<Daemon>, caller: &Caller, verb: &str, args: &[String], reason: &str) -> Response {
    let parsed = match Verb::parse(verb, args) {
        Ok(v) => v,
        Err(e) => return refusal(e),
    };
    let reason = match request::check_reason(reason) {
        Ok(r) => r,
        Err(e) => return refusal(e),
    };

    let who = origin(daemon, caller);

    // Every verb in this vocabulary is a root operation, and §7's table gives
    // a different answer for a remote origin than a local one. A request the
    // daemon cannot attribute to an origin therefore cannot be filed: the
    // human deciding it would be reading a prompt that could not tell them
    // which column of that table they were in.
    let Some(source) = who.request_origin else {
        return Response::error(
            ErrorKind::PermissionDenied,
            format!(
                "{}, so this request cannot be attributed to an origin and will not be filed",
                who.origin_unreadable
                    .unwrap_or_else(|| "the origin could not be established".to_string())
            ),
        );
    };

    let dir = request::requests_dir();
    let grants = Grants::load(&request::grants_file());
    // Two ways a request can arrive already decided, and they are different
    // things. A per-project grant (P0-013) is a standing decision a human made
    // about this project and it outlives every session. A system-access grant
    // (§4.4) is a decision a human made about THIS session, for a stated
    // window, and it dies with the session or the clock.
    //
    // The system-access half is read from the daemon's memory, never from the
    // grant store, because the session this is being decided for can write the
    // store — see `grants.rs`.
    //
    // They are recorded differently for the same reason. A project grant is a
    // standing decision, so the record says `allow_for_project` and the next
    // identical request in that project finds it again. A session grant is
    // not: it decided this one request, and the window it came from may be
    // gone by the next, so the record says `allow_once` and names the grant.
    let now = request::now_ms();
    let by_project = grants.allows(who.project.as_deref(), &parsed);
    let by_grant = daemon
        .grants
        .covering_grant(who.session, parsed.name(), now);
    let (decision, attributed, event) = decided_by(by_project, by_grant);

    let req = PrivilegeRequest {
        id: request::next_id(&dir),
        verb: parsed,
        reason,
        session: who.session,
        agent: who.agent,
        project: who.project,
        request_origin: Some(source.origin),
        origin_source: Some(source.source),
        actor: who.actor,
        decision: decision.unwrap_or(Decision::Pending),
        created_ms: now,
        decided_ms: decision.map(|_| now),
        executed_ms: None,
        exit_code: None,
        system_grant: attributed,
    };

    if let Err(e) = request::save(&dir, &req) {
        return Response::error(ErrorKind::Internal, format!("recording the request: {e}"));
    }
    let _ = request::audit(&request::audit_log(), event, &req);
    Response::Request(Box::new(req))
}

/// Every request on disk.
pub fn list() -> Response {
    match request::list(&request::requests_dir()) {
        Ok(requests) => Response::Requests { requests },
        Err(e) => Response::error(ErrorKind::Internal, format!("reading requests: {e}")),
    }
}

/// Record a human's decision.
///
/// Refuses to re-decide a request that has already been decided. Without that,
/// a denied request could be flipped to approved by anything that can reach the
/// socket — including the agent that filed it.
pub fn decide(daemon: &Arc<Daemon>, caller: &Caller, id: u32, decision: Decision) -> Response {
    // A session may not decide its own requests, or the approval step is
    // decoration. Only an unsessioned peer — the human's shell, or the shell
    // itself — may decide.
    let who = origin(daemon, caller);
    if let Some(session) = who.session {
        return Response::error(
            ErrorKind::PermissionDenied,
            format!(
                "session {session} cannot decide its own privilege request; \
                 a human decides with `rime request allow|deny`"
            ),
        );
    }

    // §7, both columns: "Root capability — Local: local auth. Claude Remote
    // Control: local approval required." Every verb in the vocabulary is a
    // root capability, so both columns end in the same place — a human at
    // this machine — and this is the check that makes that true.
    //
    // Without it, a systemd user timer running `rime request allow 3` is an
    // unattended approval of root: it is not a session, so the check above
    // lets it through, and it is not a human either. That is the hole this
    // closes, and it is the whole of "root/unsafe mode requires local
    // approval by default".
    //
    // Refused rather than defaulted when the origin could not be established.
    // The message names what could not be read, because the alternative to
    // being able to act on it is a machine on which nothing can be approved
    // and nothing says why.
    let Some(decider) = who.request_origin else {
        return Response::error(
            ErrorKind::PermissionDenied,
            format!(
                "{}, so this connection cannot be shown to be a human at this machine — and \
                 §7 reserves approving a root operation for one",
                who.origin_unreadable
                    .unwrap_or_else(|| "the origin could not be established".to_string())
            ),
        );
    };
    if !decider.is_local() {
        return Response::error(
            ErrorKind::PermissionDenied,
            format!(
                "a {} request cannot approve a root operation; §7 reserves that for a human at \
                 this machine, whichever origin filed it. Approve it from a terminal or from \
                 the Agent Center",
                decider.origin
            ),
        );
    }
    if decision == Decision::Pending {
        return Response::error(
            ErrorKind::BadRequest,
            "'pending' is not a decision".to_string(),
        );
    }

    let dir = request::requests_dir();
    let mut req = match request::load(&dir, id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Response::error(ErrorKind::NoSuchRequest, format!("no request {id}"));
        }
        Err(e) => return Response::error(ErrorKind::Internal, format!("reading request {id}: {e}")),
    };
    if req.decision != Decision::Pending {
        return Response::error(
            ErrorKind::BadRequest,
            format!(
                "request {id} was already {}; file a new one rather than \
                 changing a recorded decision",
                req.decision.as_str()
            ),
        );
    }

    req.decision = decision;
    req.decided_ms = Some(request::now_ms());

    if decision == Decision::AllowForProject {
        match req.project.clone() {
            Some(project) => {
                let path = request::grants_file();
                let mut grants = Grants::load(&path);
                grants.allow(&project, &req.verb);
                if let Err(e) = grants.save(&path) {
                    return Response::error(
                        ErrorKind::Internal,
                        format!("recording the grant: {e}"),
                    );
                }
            }
            None => {
                // Nothing to scope the grant to. Downgraded rather than
                // silently stored globally: a grant with no project is a grant
                // for every project, which is not what the user chose.
                req.decision = Decision::AllowOnce;
            }
        }
    }

    if let Err(e) = request::save(&dir, &req) {
        return Response::error(ErrorKind::Internal, format!("saving request {id}: {e}"));
    }
    let _ = request::audit(&request::audit_log(), "decided", &req);
    Response::Request(Box::new(req))
}

/// Mark an approved request as executed, recording its exit status.
///
/// Called by `rime request approve` after it has run the operation as root.
/// Refuses a request that is not approved, and refuses one that has already
/// run — an approval is for one execution.
pub fn executed(id: u32, exit_code: i32) -> Response {
    let dir = request::requests_dir();
    let mut req = match request::load(&dir, id) {
        Ok(Some(r)) => r,
        Ok(None) => return Response::error(ErrorKind::NoSuchRequest, format!("no request {id}")),
        Err(e) => return Response::error(ErrorKind::Internal, format!("reading request {id}: {e}")),
    };
    if !req.decision.is_allowed() {
        return Response::error(
            ErrorKind::PermissionDenied,
            format!("request {id} is {}, not approved", req.decision.as_str()),
        );
    }
    if req.executed_ms.is_some() {
        return Response::error(
            ErrorKind::BadRequest,
            format!("request {id} has already been executed"),
        );
    }
    req.executed_ms = Some(request::now_ms());
    req.exit_code = Some(exit_code);
    if let Err(e) = request::save(&dir, &req) {
        return Response::error(ErrorKind::Internal, format!("saving request {id}: {e}"));
    }
    let _ = request::audit(&request::audit_log(), "executed", &req);
    Response::Request(Box::new(req))
}

/// The recorded grants.
pub fn grants() -> Response {
    let g = Grants::load(&request::grants_file());
    Response::Grants {
        projects: g.projects,
    }
}

/// Drop a grant, or every grant for a project when `key` is `None`.
pub fn revoke(daemon: &Arc<Daemon>, caller: &Caller, project: &str, key: Option<&str>) -> Response {
    // Revoking is a policy change, so it is subject to the same rule as
    // deciding: a session cannot widen or narrow its own permissions.
    if let Some(session) = origin(daemon, caller).session {
        return Response::error(
            ErrorKind::PermissionDenied,
            format!("session {session} cannot change its own grants"),
        );
    }
    let path = request::grants_file();
    let mut g = Grants::load(&path);
    let removed = match key {
        Some(k) => usize::from(g.revoke(project, k)),
        None => g.revoke_project(project),
    };
    if removed == 0 {
        return Response::error(
            ErrorKind::NoSuchRequest,
            format!("nothing granted for {project}"),
        );
    }
    if let Err(e) = g.save(&path) {
        return Response::error(ErrorKind::Internal, format!("saving grants: {e}"));
    }
    Response::Grants {
        projects: g.projects,
    }
}

/// Turn a validation failure into a response. Always `BadRequest`: these are
/// all "you asked for something that is not askable", never internal faults.
fn refusal(e: RequestError) -> Response {
    Response::error(ErrorKind::BadRequest, e.to_string())
}

/// Ensure the state directory exists, so a first request does not fail on a
/// missing path. `0700`, because the pending requests are a description of what
/// privileged operations this machine is about to run.
pub fn ensure_dirs() {
    let _ = paths::ensure_private_dir(&request::requests_dir());
}

#[cfg(test)]
mod tests {
    use super::*;
    use rime_agent_core::origin::SessionOrigin;

    /// A caller with no peer, carrying a latch. Enough for
    /// [`connection_origin`], which never touches the peer itself.
    fn latched(origin: RequestOrigin, actor: Option<&str>) -> Caller {
        let mut c = Caller::new(None);
        c.latch(origin, actor.map(|a| a.to_string()));
        c
    }

    #[test]
    fn a_latch_narrows_an_observation_and_says_it_was_declared() {
        let who = connection_origin(
            Ok(RequestOrigin::LocalTerminal),
            &latched(RequestOrigin::RemoteControl, Some("pixel-8")),
        );
        let got = who.request_origin.expect("an origin");
        assert_eq!(got.origin, RequestOrigin::RemoteControl);
        assert_eq!(got.source, OriginSource::Declared);
        assert_eq!(who.actor.as_deref(), Some("pixel-8"));
    }

    #[test]
    fn a_latch_that_would_loosen_is_ignored_and_the_observation_stands() {
        // The use-time half of the property. A latch set when the peer was a
        // login session must not survive the peer becoming something the
        // narrowing no longer narrows — here `mcp` over `claude-remote-control`,
        // which drops the lock gate.
        let who = connection_origin(
            Ok(RequestOrigin::RemoteControl),
            &latched(RequestOrigin::Mcp, Some("pixel-8")),
        );
        let got = who.request_origin.expect("an origin");
        assert_eq!(got.origin, RequestOrigin::RemoteControl);
        assert_eq!(got.source, OriginSource::Observed);
        assert_eq!(
            who.actor, None,
            "an actor survived a declaration that did not"
        );
    }

    #[test]
    fn the_pair_a_remote_proxy_actually_declares_is_accepted() {
        // `rime-remoted` runs as a systemd user service, so what it declares
        // in production is `scheduled-job` -> `claude-remote-control`. Every
        // test that drives it declares from a login session instead, because
        // that is what `cargo test` is — so if this pair ever stopped being
        // accepted, the feature would be dead in production while the whole
        // suite stayed green.
        //
        // It has to be accepted for a reason worth stating: `scheduled-job`
        // is already non-local, so the declaration buys no elevation. What it
        // buys is the LOCK GATE, which `scheduled-job` does not carry and
        // Remote Control does — a human elsewhere driving the machine past a
        // locked screen is the case §7 says the owner is asked about.
        let who = connection_origin(
            Ok(RequestOrigin::ScheduledJob),
            &latched(RequestOrigin::RemoteControl, Some("pixel-8")),
        );
        let got = who.request_origin.expect("an origin");
        assert_eq!(got.origin, RequestOrigin::RemoteControl);
        assert_eq!(got.source, OriginSource::Declared);
        assert!(!got.origin.is_local(), "the production pair reached a local origin");
        assert!(
            got.origin.lock_gated() && !RequestOrigin::ScheduledJob.lock_gated(),
            "the declaration did not take on the lock gate, which is what it is for"
        );
        assert_eq!(who.actor.as_deref(), Some("pixel-8"));
    }

    #[test]
    fn no_latch_leaves_the_observation_exactly_as_it_was() {
        for o in RequestOrigin::ALL {
            let who = connection_origin(Ok(*o), &Caller::new(None));
            let got = who.request_origin.expect("an origin");
            assert_eq!(got.origin, *o);
            assert_eq!(got.source, OriginSource::Observed);
            assert!(who.actor.is_none());
        }
    }

    #[test]
    fn a_latch_cannot_rescue_an_observation_that_could_not_be_read() {
        // The defect class this codebase keeps finding: "could not tell" must
        // not become an answer. A declaration over an unreadable observation
        // has no basis to be more restricted than, so there is nothing to
        // apply it to.
        let who = connection_origin(
            Err("/proc/1234/cgroup: no such file".into()),
            &latched(RequestOrigin::RemoteControl, None),
        );
        assert!(who.request_origin.is_none());
        assert!(who
            .origin_unreadable
            .as_deref()
            .unwrap_or_default()
            .contains("cgroup"));
    }

    #[test]
    fn no_latch_can_ever_produce_a_local_origin() {
        // Over the whole product, not over the pairs someone thought of.
        // `declare_on_connection` refuses a local wanted by name before it
        // latches, so this asserts the property from the other side: even if
        // one were latched, applying it must not produce a local origin from
        // a non-local observation.
        for observed in RequestOrigin::ALL {
            for wanted in RequestOrigin::ALL {
                let who = connection_origin(Ok(*observed), &latched(*wanted, None));
                let got = who.request_origin.expect("an origin").origin;
                assert!(
                    !got.is_local() || observed.is_local(),
                    "{observed} + a latch of {wanted} produced the local origin {got}"
                );
            }
        }
    }

    #[test]
    fn an_actor_is_refused_for_the_reasons_it_is_refused_for() {
        assert!(unprintable_actor("pixel-8-office").is_none());
        assert!(unprintable_actor("Andre's phone (kitchen)").is_none());
        assert!(unprintable_actor("").is_some(), "an empty actor");
        assert!(
            unprintable_actor(&"a".repeat(MAX_ACTOR)).is_none(),
            "the bound is inclusive"
        );
        assert!(unprintable_actor(&"a".repeat(MAX_ACTOR + 1)).is_some());
        for bad in ["a\nb", "a\rb", "a\u{1b}[2Kb", "a\tb", "a\0b"] {
            assert!(
                unprintable_actor(bad).is_some(),
                "{bad:?} was accepted onto a root approval prompt"
            );
        }
        // Counted in characters, not bytes: a name in a non-Latin script must
        // not be refused for being long when it is not.
        assert!(unprintable_actor(&"é".repeat(MAX_ACTOR)).is_none());
    }

    fn unsessioned(origin: RequestOrigin) -> Origin {
        Origin {
            request_origin: Some(SessionOrigin::observed(origin)),
            ..Origin::default()
        }
    }

    #[test]
    fn a_person_at_a_terminal_may_write_into_a_session() {
        // The whole point of the verb. A connection this daemon classified and
        // found outside every session is the shell's push-to-talk route or a
        // person running `rime agent input`, and it is allowed.
        let who = unsessioned(RequestOrigin::LocalTerminal);
        assert!(refuse_input(&who, 4).is_none());
    }

    #[test]
    fn a_session_may_not_write_into_another_session() {
        // Dimension 3 leaking sideways: an agent that could type into a
        // sibling could ask it to run what the sibling was granted and the
        // caller was not. The refusal names both ids, because the interesting
        // half of the log line is WHICH session tried.
        let who = Origin {
            session: Some(7),
            ..Origin::default()
        };
        let refusal = refuse_input(&who, 4).expect("a session must be refused");
        let (kind, message) = refusal.as_error().expect("an error");
        assert_eq!(kind, ErrorKind::PermissionDenied);
        assert!(message.contains("session 7"), "{message}");
        assert!(message.contains("session 4"), "{message}");
    }

    #[test]
    fn a_session_may_not_write_into_itself_either() {
        // Not a special case worth allowing. An agent typing into its own
        // terminal is an agent writing its own next prompt, which is the same
        // borrowed-authority problem with one hop taken out, and there is no
        // caller that wants it: an agent already owns its stdout.
        let who = Origin {
            session: Some(4),
            ..Origin::default()
        };
        assert!(refuse_input(&who, 4).is_some());
    }

    #[test]
    fn a_connection_that_could_not_be_classified_is_refused_and_not_assumed_human() {
        // The important one. `session: None` on an unreadable origin means
        // "the walk never happened", not "the walk happened and found
        // nothing" — the two are indistinguishable in that field, which is why
        // the reason field is consulted instead of the absence. Reading a
        // failed classification as a human is the defect this repository has
        // already shipped once, in the other direction.
        let who = Origin::unreadable("the kernel would not report the peer credentials");
        assert!(who.session.is_none(), "the fixture must exercise the gap");
        let refusal = refuse_input(&who, 4).expect("an unclassified caller must be refused");
        let (kind, message) = refusal.as_error().expect("an error");
        assert_eq!(kind, ErrorKind::PermissionDenied);
        assert!(
            message.contains("could not be classified"),
            "the refusal must say why it could not decide: {message}"
        );
    }

    #[test]
    fn a_person_may_read_this_machines_clipboard_and_an_agent_may_not() {
        // Both directions in one test on purpose. A refusal predicate asserted
        // only in the refusing direction is indistinguishable from `|_| Some`,
        // which refuses the caller the verb exists for and passes every test
        // anybody wrote for it.
        //
        // The allowed caller is a paired phone. `refuse_input`'s own test uses
        // a local terminal for this, and using a REMOTE origin here is the
        // claim that matters: this verb is not `inject`, it does not refuse a
        // non-local origin, and a version of it that did would refuse the only
        // caller it has. `claude-remote-control` is the origin `rime-remoted`
        // declares for a paired device — see `proxy.rs:139` and the note there
        // about why an eighth origin was not added.
        let phone = unsessioned(RequestOrigin::RemoteControl);
        assert!(
            refuse_clipboard(&phone).is_none(),
            "a paired device is the caller this verb exists for"
        );
        assert!(
            refuse_clipboard(&unsessioned(RequestOrigin::LocalTerminal)).is_none(),
            "and a person at the machine is not refused either"
        );

        // The refusal. A person copies a password out of a password manager
        // and pastes it thirty seconds later; an agent that could ask this
        // would have a window on every one of those, outside every sandbox,
        // with nothing on screen and no grant spent.
        let agent = Origin {
            session: Some(7),
            ..Origin::default()
        };
        let refusal = refuse_clipboard(&agent).expect("a session must be refused");
        let (kind, message) = refusal.as_error().expect("an error");
        assert_eq!(kind, ErrorKind::PermissionDenied);
        assert!(
            message.contains("session 7"),
            "the refusal must name which session asked: {message}"
        );
        assert!(
            message.contains("clipboard"),
            "and what it was refused: {message}"
        );
    }

    #[test]
    fn an_unclassified_caller_may_not_read_this_machines_clipboard_either() {
        // `session: None` here means "the walk never happened", not "the walk
        // happened and found nothing". Reading a failed classification as a
        // human is how an agent gets the password.
        let who = Origin::unreadable("the kernel would not report the peer credentials");
        assert!(who.session.is_none(), "the fixture must exercise the gap");
        let refusal = refuse_clipboard(&who).expect("an unclassified caller must be refused");
        let (kind, message) = refusal.as_error().expect("an error");
        assert_eq!(kind, ErrorKind::PermissionDenied);
        assert!(
            message.contains("could not be classified"),
            "the refusal must say why it could not decide: {message}"
        );
    }

    #[test]
    fn the_clipboard_gate_and_the_input_gate_agree_on_every_origin() {
        // The claim `refuse_clipboard`'s documentation makes — "the same rule,
        // different sentence" — asserted rather than left as prose. Two gates
        // that were written to agree drift when one of them is edited, and
        // the drift is silent: each keeps passing its own tests.
        //
        // Only the VERDICTS are compared. The messages differ deliberately,
        // because the sentence is what the person reads.
        let mut cases: Vec<(String, Origin)> = vec![
            (
                "a session".to_string(),
                Origin {
                    session: Some(3),
                    ..Origin::default()
                },
            ),
            (
                "an unreadable origin".to_string(),
                Origin::unreadable("peer credentials unavailable"),
            ),
            ("the default origin".to_string(), Origin::default()),
        ];
        // Every §7 origin, taken from the enum rather than from a list written
        // here: an eighth origin added later is covered without anybody
        // remembering to come back to this test.
        for o in RequestOrigin::ALL {
            cases.push((o.as_str().to_string(), unsessioned(*o)));
        }
        for (what, who) in cases {
            assert_eq!(
                refuse_clipboard(&who).is_some(),
                refuse_input(&who, 4).is_some(),
                "the two gates disagree about {what}, so one of them has been edited \
                 and the other has not"
            );
            // `rename` and `peek` are the same rule too (§1.5, §1.6): only a
            // person names a session, and agents do not read each other's
            // screens.
            assert_eq!(
                refuse_rename(&who, 4).is_some(),
                refuse_input(&who, 4).is_some(),
                "rename and input disagree about {what}"
            );
            assert_eq!(
                refuse_peek(&who, 4).is_some(),
                refuse_input(&who, 4).is_some(),
                "peek and input disagree about {what}"
            );
        }
    }

    #[test]
    fn rename_and_peek_refusals_say_which_verb_was_refused() {
        // The same verdicts as `input`, but not the same sentence: a person
        // told they may not "write into the terminal" after tapping Rename
        // would look for the wrong thing.
        let session = Origin {
            session: Some(3),
            ..Origin::default()
        };
        let (_, rename) = refuse_rename(&session, 4).unwrap().as_error().map(|(k, m)| (k, m.to_string())).unwrap();
        assert!(rename.contains("rename session 4"), "{rename}");
        let (kind, peek) = refuse_peek(&session, 4).unwrap().as_error().map(|(k, m)| (k, m.to_string())).unwrap();
        assert_eq!(kind, ErrorKind::PermissionDenied);
        assert!(peek.contains("screen"), "{peek}");
        let unread = Origin::unreadable("peer credentials unavailable");
        let (_, why) = refuse_rename(&unread, 4).unwrap().as_error().map(|(k, m)| (k, m.to_string())).unwrap();
        assert!(why.contains("peer credentials unavailable"), "{why}");
        // And a person is let through by both.
        assert!(refuse_rename(&unsessioned(RequestOrigin::LocalTerminal), 4).is_none());
        assert!(refuse_peek(&unsessioned(RequestOrigin::RemoteControl), 4).is_none());
    }

    #[test]
    fn only_a_remote_control_origin_is_a_phone_for_the_terminal_size() {
        use rime_agent_core::session::Viewer;
        for o in RequestOrigin::ALL {
            let want = if *o == RequestOrigin::RemoteControl {
                Viewer::Remote
            } else {
                Viewer::Local
            };
            assert_eq!(viewer(&unsessioned(*o)), want, "{o}");
        }
        // A session's inherited origin counts the same way as an observed one:
        // the class is the origin's, not the connection's shape.
        let remote_session = Origin {
            session: Some(3),
            request_origin: Some(SessionOrigin::inherited(RequestOrigin::RemoteControl)),
            ..Origin::default()
        };
        assert_eq!(viewer(&remote_session), Viewer::Remote);
        // No origin at all is not a phone: rime-remoted cannot reach agentd
        // without an accepted declaration, so an unreadable origin is never
        // one of its connections.
        assert_eq!(viewer(&Origin::unreadable("x")), Viewer::Local);
        assert_eq!(viewer(&Origin::default()), Viewer::Local);
    }

    #[test]
    fn a_request_a_session_grant_covered_is_not_recorded_as_a_standing_grant() {
        // §4.4's grant is a window, not a policy. If a request it covered were
        // filed as `allow_for_project`, the audit would name an authority no
        // human created: a standing permission for that project, readable by
        // anyone who lists grants later, backed by nothing.
        let (decision, attributed, event) = decided_by(false, Some(9));
        assert_eq!(decision, Some(Decision::AllowOnce));
        assert_eq!(attributed, Some(9), "the trail must name the grant");
        assert_eq!(event, "requested-and-covered");

        // A standing grant is the other authority, and says so.
        let (decision, attributed, event) = decided_by(true, None);
        assert_eq!(decision, Some(Decision::AllowForProject));
        assert_eq!(attributed, None);
        assert_eq!(event, "requested-and-granted");

        // Both: the human's standing decision is the reason it was allowed,
        // and it would have been allowed with no window open, so the record
        // does not hang it on a window that will be gone in an hour.
        let (decision, attributed, event) = decided_by(true, Some(9));
        assert_eq!(decision, Some(Decision::AllowForProject));
        assert_eq!(attributed, None, "a grant that decided nothing is not cited");
        assert_eq!(event, "requested-and-granted");

        // Neither: a human still has to look at it.
        assert_eq!(decided_by(false, None), (None, None, "requested"));
    }

    #[test]
    fn a_connection_inside_a_session_is_refused_whatever_its_origin_says() {
        // P0-007's fourth criterion as the exhaustive claim it has to be. The
        // session check comes first, so it holds over every origin — including
        // `local-terminal`, which is what a session's own connection would
        // classify as if it were classified afresh rather than inherited.
        for origin in RequestOrigin::ALL {
            let who = Origin {
                session: Some(4),
                ..unsessioned(*origin)
            };
            let err = may_be_granted(&who, "ask for a grant").expect_err("must refuse");
            assert_eq!(
                err,
                GrantError::FromInsideASession {
                    session: 4,
                    what: "ask for a grant"
                },
                "{origin}"
            );
        }
    }

    #[test]
    fn only_a_local_origin_outside_a_session_may_be_granted_anything() {
        // §7's table, at the gate. The two local origins pass; the five that
        // mean "nobody is at this keyboard" do not, and that includes the two
        // that run on this machine — a scheduled job and a subagent.
        for origin in RequestOrigin::ALL {
            let who = unsessioned(*origin);
            match may_be_granted(&who, "ask for a grant") {
                Ok(got) => {
                    assert!(origin.is_local(), "{origin} was granted the local column");
                    assert_eq!(got, *origin);
                }
                Err(GrantError::NotLocal { origin: got, .. }) => {
                    assert!(!origin.is_local(), "{origin} was refused the local column");
                    assert_eq!(got, *origin);
                }
                Err(other) => panic!("{origin}: {other}"),
            }
        }
    }

    #[test]
    fn an_origin_that_could_not_be_read_is_refused_rather_than_defaulted() {
        // `RequestOrigin::default()` is `local-terminal`, which is exactly the
        // column being asked for, so a fallback here would hand a grant to a
        // connection nobody identified.
        let who = Origin {
            origin_unreadable: Some("/proc/9/cgroup: no such file or directory".into()),
            ..Origin::default()
        };
        let err = may_be_granted(&who, "ask for a grant").expect_err("must refuse");
        assert!(matches!(err, GrantError::OriginUnknown(_)), "{err}");
        assert!(err.to_string().contains("/proc/9/cgroup"), "{err}");

        // And with nothing recorded at all, which is the shape a future caller
        // could arrive in.
        assert!(may_be_granted(&Origin::default(), "ask for a grant").is_err());
    }

    #[test]
    fn the_session_check_beats_the_origin_check_so_the_refusal_names_the_session() {
        // Order matters for what the user is told. A session whose origin is
        // also non-local must hear "a session cannot do this", which is
        // actionable, rather than "a subagent cannot do this", which invites
        // the wrong fix.
        let who = Origin {
            session: Some(9),
            ..unsessioned(RequestOrigin::Subagent)
        };
        let err = may_be_granted(&who, "renew a system-access grant").expect_err("refuse");
        assert!(err.to_string().contains("session 9"), "{err}");
        assert!(err.to_string().contains("renew a system-access grant"), "{err}");
    }

    // ---------------------------------------------------------------------
    // decide_origin — §7's two columns (P0-014, commit 4)
    // ---------------------------------------------------------------------

    use rime_agent_core::webauthn::test_support::{Signer, UP, UP_UV};
    use rime_agent_core::webauthn::Challenge;
    use std::cell::Cell;

    /// What a client sends. The contents never matter to `decide_origin`:
    /// every test here supplies the redeem as a closure, because the point of
    /// that closure being an argument is that the decision can be checked
    /// without a daemon, a store or a socket.
    fn submitted() -> SubmittedFactor {
        SubmittedFactor {
            nonce: "a-nonce".into(),
            credential: "the owner's key".into(),
            assertion: "four lines from fido2-assert".into(),
        }
    }

    /// A redeem that must not be reached, and says so if it is.
    fn never_redeemed(_: &SubmittedFactor) -> Result<SecondFactor, AssertionError> {
        panic!("this caller was refused before the gate; its challenge must not be spent")
    }

    fn remote_origins() -> impl Iterator<Item = RequestOrigin> {
        RequestOrigin::ALL.iter().copied().filter(|o| !o.is_local())
    }

    #[test]
    fn the_local_column_is_not_asked_for_a_key_whatever_the_owner_set() {
        // §7 gives root "local auth" locally, and that path is
        // `may_be_granted` plus polkit. `decide_origin` must not become a
        // second, weaker copy of it: neither the policy nor the presence of a
        // key may change the answer for a local origin, or the local path has
        // silently acquired a requirement §7 does not give it.
        for origin in RequestOrigin::ALL.iter().copied().filter(|o| o.is_local()) {
            for policy in OriginPolicy::ALL.iter().copied() {
                for kind in [GrantKind::SystemAccess, GrantKind::BreakGlass] {
                    let e = Elevating { policy, scope: None, ttl_ms: 900_000, factor: None };
                    // `Approved::Local` and not merely `Ok`: the point is
                    // that the local column is sent to polkit, and a
                    // `ByKey` here would mean a local caller had silently
                    // stopped being asked for a password.
                    assert_eq!(
                        decide_origin(&unsessioned(origin), "ask", kind, &e, never_redeemed),
                        Ok(Approved::Local(origin)),
                        "{origin} under {policy}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_connection_inside_a_session_is_refused_before_its_challenge_is_spent() {
        // Step 1 of `may_be_granted` still decides, and still first. An agent
        // cannot buy itself root with a touch — and it does not even get to
        // spend the nonce finding that out, which matters because a spent
        // challenge is a challenge the human has to collect again.
        let s = submitted();
        for origin in RequestOrigin::ALL.iter().copied() {
            let who = Origin { session: Some(4), ..unsessioned(origin) };
            let e = Elevating {
                policy: OriginPolicy::RemoteElevationAllowed,
                scope: None,
                ttl_ms: 900_000,
                factor: Some(&s),
            };
            let err = decide_origin(&who, "ask for a grant", GrantKind::BreakGlass, &e, never_redeemed)
                .expect_err("must refuse");
            assert_eq!(
                err,
                GrantError::FromInsideASession { session: 4, what: "ask for a grant" },
                "{origin}"
            );
        }
    }

    #[test]
    fn an_origin_that_could_not_be_established_is_refused_before_the_gate_too() {
        // The other `may_be_granted` refusal that must survive P0-014: a
        // connection nobody could classify is not offered the chance to try a
        // key. Only `NotLocal` is carried forward.
        let s = submitted();
        let who = Origin::default(); // no request_origin at all
        let e = Elevating {
            policy: OriginPolicy::RemoteElevationAllowed,
            scope: None,
            ttl_ms: 900_000,
            factor: Some(&s),
        };
        let err = decide_origin(&who, "ask for a grant", GrantKind::BreakGlass, &e, never_redeemed)
            .expect_err("must refuse");
        assert!(
            !matches!(err, GrantError::RemoteElevation(_) | GrantError::SecondFactorRefused(_)),
            "an unclassifiable connection was sent to the second-factor gate: {err}"
        );
    }

    #[test]
    fn a_remote_origin_under_the_default_policy_is_told_the_setting_and_not_the_key() {
        // The order `may_elevate` documents, asserted from the caller's side:
        // the policy is checked BEFORE the factor, so a user who never opted
        // in reads a sentence about the setting they have to change rather
        // than about a key they were never going to be asked for.
        for origin in remote_origins() {
            let e = Elevating {
                policy: OriginPolicy::LocalElevationOnly,
                scope: None,
                ttl_ms: 900_000,
                factor: None,
            };
            let err = decide_origin(&unsessioned(origin), "ask", GrantKind::SystemAccess, &e, never_redeemed)
                .expect_err("must refuse");
            assert_eq!(
                err,
                GrantError::RemoteElevation(RemoteElevationRefused::PolicyForbids { origin }),
                "{origin}"
            );
            assert!(err.to_string().contains("--origin-policy remote"), "{err}");
        }
    }

    #[test]
    fn a_key_that_answered_badly_under_a_forbidding_policy_still_reads_as_the_policy() {
        // The same ordering, now with a key in play and the harder direction:
        // a bad assertion must NOT be what the user is told about when the
        // owner never opted in, because fixing the key would not help them.
        // The nonce is spent anyway — one issue, one attempt — and that is
        // asserted rather than left to be discovered.
        for origin in remote_origins() {
            let s = submitted();
            let spent = Cell::new(0);
            let e = Elevating {
                policy: OriginPolicy::LocalElevationOnly,
                scope: None,
                ttl_ms: 900_000,
                factor: Some(&s),
            };
            let err = decide_origin(&unsessioned(origin), "ask", GrantKind::SystemAccess, &e, |_| {
                spent.set(spent.get() + 1);
                Err(AssertionError::BadSignature)
            })
            .expect_err("must refuse");
            assert_eq!(
                err,
                GrantError::RemoteElevation(RemoteElevationRefused::PolicyForbids { origin }),
                "{origin}"
            );
            assert_eq!(spent.get(), 1, "the challenge is spent exactly once, {origin}");
        }
    }

    #[test]
    fn a_remote_origin_the_owner_opted_in_for_and_nobody_answered_is_told_what_is_missing() {
        for origin in remote_origins() {
            let e = Elevating {
                policy: OriginPolicy::RemoteElevationAllowed,
                scope: None,
                ttl_ms: 900_000,
                factor: None,
            };
            let err = decide_origin(&unsessioned(origin), "ask", GrantKind::SystemAccess, &e, never_redeemed)
                .expect_err("must refuse");
            assert_eq!(
                err,
                GrantError::RemoteElevation(RemoteElevationRefused::NoSecondFactor { origin }),
                "{origin}"
            );
        }
    }

    #[test]
    fn a_key_that_answered_and_did_not_check_out_is_told_so_and_not_told_nothing_answered() {
        // `may_elevate` can only ever say "nothing answered", because a
        // refused assertion never became a receipt. The whole reason
        // `decide_origin` keeps the `Result` around is to tell the two apart,
        // and the sentence has to mention that the challenge is gone.
        for why in [
            AssertionError::BadSignature,
            AssertionError::NoUserPresence,
            AssertionError::NoSuchChallenge,
            AssertionError::CounterWentBackwards { stored: 9, presented: 2 },
        ] {
            let s = submitted();
            let e = Elevating {
                policy: OriginPolicy::RemoteElevationAllowed,
                scope: None,
                ttl_ms: 900_000,
                factor: Some(&s),
            };
            let err = decide_origin(
                &unsessioned(RequestOrigin::RemoteControl),
                "ask",
                GrantKind::SystemAccess,
                &e,
                |_| Err(why.clone()),
            )
            .expect_err("must refuse");
            assert_eq!(err, GrantError::SecondFactorRefused(why.clone()), "{why}");
            assert!(err.to_string().contains("spent"), "{err}");
        }
    }

    // -- the branch that needs a key that really signed ---------------------

    /// A receipt for exactly that elevation, minted the way the daemon mints
    /// one: a real ECDSA signature over the real challenge binding, checked by
    /// the shipped verifier. See `webauthn::test_support` for why a signer is
    /// exposed and a `SecondFactor` constructor is not.
    fn receipt(signer: &Signer, scope: Option<u32>, kind: GrantKind, ttl_ms: u64, flags: u8) -> SecondFactor {
        let c = Challenge::with_nonce(scope, kind, ttl_ms, 1_000, b"a-nonce-for-this-test");
        signer.receipt(&c, flags, 1)
    }

    #[test]
    fn a_remote_origin_the_owner_opted_in_for_is_let_through_by_a_key_that_really_signed() {
        // The branch this whole commit exists for, and the only one that
        // cannot be written without a signer. Both callers are covered: a
        // session being started (`scope: None`, the primary case, because
        // `authorise_grant` runs before the id is reserved) and a renewal of
        // an existing grant (`scope: Some(..)`).
        let signer = Signer::new("the owner's key");
        for origin in remote_origins() {
            for (scope, kind, ttl_ms) in [
                (None, GrantKind::SystemAccess, 900_000u64),
                (Some(7), GrantKind::BreakGlass, 60_000),
            ] {
                let s = submitted();
                let factor = receipt(&signer, scope, kind, ttl_ms, UP_UV);
                let e = Elevating {
                    policy: OriginPolicy::RemoteElevationAllowed,
                    scope,
                    ttl_ms,
                    factor: Some(&s),
                };
                let want = factor.clone();
                let got = decide_origin(&unsessioned(origin), "ask", kind, &e, move |_| Ok(factor));
                // `ByKey`, carrying the very receipt that was minted. A
                // `Local` here would mean the non-local column had been let
                // through to polkit, which for a remote caller is a refusal
                // no password can lift.
                assert_eq!(
                    got,
                    Ok(Approved::ByKey(origin, want)),
                    "{origin} / {kind:?} / scope {scope:?}"
                );
            }
        }
    }

    #[test]
    fn a_touch_answers_for_one_elevation_and_not_for_a_neighbouring_one() {
        // `may_answer_for` compares session, kind and ttl, and the session in
        // BOTH directions. Each row mints a real receipt for one elevation and
        // presents it for another, so none of these refusals comes from a
        // signature that failed — they come from consent that was given for
        // something else.
        let signer = Signer::new("the owner's key");
        let start = (None, GrantKind::SystemAccess, 900_000u64);
        for (touched, asked) in [
            // a touch collected while starting a session cannot renew one
            (start, (Some(7), GrantKind::SystemAccess, 900_000)),
            // and a touch for session 7 cannot start a new session
            ((Some(7), GrantKind::SystemAccess, 900_000), start),
            // nor does session 7's touch answer for session 8
            ((Some(7), GrantKind::SystemAccess, 900_000), (Some(8), GrantKind::SystemAccess, 900_000)),
            // capability-scoped root is not break-glass root
            (start, (None, GrantKind::BreakGlass, 900_000)),
            // and a window is part of what was consented to
            (start, (None, GrantKind::SystemAccess, 8 * 3_600_000)),
        ] {
            let s = submitted();
            let factor = receipt(&signer, touched.0, touched.1, touched.2, UP_UV);
            let e = Elevating {
                policy: OriginPolicy::RemoteElevationAllowed,
                scope: asked.0,
                ttl_ms: asked.2,
                factor: Some(&s),
            };
            let err = decide_origin(
                &unsessioned(RequestOrigin::RemoteControl),
                "ask",
                asked.1,
                &e,
                move |_| Ok(factor),
            )
            .expect_err("a touch for something else must not answer");
            // Refused by the gate over what was consented to, NOT reported as
            // a broken signature: the key did sign, and telling the user their
            // key is bad would send them to fix the wrong thing.
            assert!(
                matches!(err, GrantError::RemoteElevation(_)),
                "touched {touched:?} asked {asked:?}: {err}"
            );
        }
    }

    // ---------------------------------------------------------------------
    // declare_origin answers the same way wherever it runs
    // ---------------------------------------------------------------------

    #[test]
    fn a_local_origin_is_refused_by_name_even_where_the_caller_cannot_be_observed() {
        // The regression test for a real CI failure, reproduced here rather
        // than only on a runner. `Caller::new(None)` is a connection whose
        // peer the kernel would not report, so `origin()` cannot classify it
        // — which is exactly the state a GitHub runner is in, its cgroup
        // being `/system.slice/hosted-compute-agent.service`: neither a login
        // session nor a user service, so `classify` answers None.
        //
        // The rule under test does not depend on any of that.
        // `may_be_declared` is a function of the wanted value alone, and the
        // two local origins are refused by name whoever is asking. Before the
        // reordering in `declare`, this came back "could not read /proc/…",
        // which is a true sentence about the machine and not an answer to
        // what was asked.
        let (daemon, _calls) = daemon_that_refuses_every_password();
        for name in ["local-terminal", "rime-shell"] {
            let mut caller = Caller::new(None);
            let reply = declare(&daemon, &mut caller, name, None);
            let (kind, message) = reply.as_error().expect("must refuse");
            assert_eq!(kind, ErrorKind::PermissionDenied, "{name}: {message}");
            assert!(
                message.contains(name),
                "the refusal must name the value that was refused, not the environment \
                 the daemon happens to be in: {message}"
            );
        }
    }

    #[test]
    fn an_actor_that_could_rewrite_a_prompt_is_refused_before_anybody_is_classified() {
        // The other CI failure, and the same cause. The actor is printed on
        // the prompt a human reads before handing out root, so a newline in
        // it is a second line of that prompt. Whether it is well formed is a
        // property of the string, so it is answered without reference to who
        // sent it — and `bad_request` is the kind, not `permission_denied`.
        let (daemon, _calls) = daemon_that_refuses_every_password();
        let mut caller = Caller::new(None);
        let reply = declare(
            &daemon,
            &mut caller,
            "claude-remote-control",
            Some("phone\nAPPROVED".to_string()),
        );
        let (kind, message) = reply.as_error().expect("must refuse");
        assert_eq!(kind, ErrorKind::BadRequest, "{message}");
    }

    // ---------------------------------------------------------------------
    // The key REPLACES polkit for the non-local column (P0-014, commit 5)
    // ---------------------------------------------------------------------

    /// An authenticator that refuses everything and counts being asked.
    ///
    /// Refusing rather than authorising on purpose: a test whose stub says yes
    /// cannot tell "polkit was not asked" from "polkit was asked and agreed",
    /// and those are the two things this whole commit is about.
    struct CountingRefusal(Arc<std::sync::atomic::AtomicUsize>);

    impl rime_agent_core::auth::Authenticator for CountingRefusal {
        fn check(
            &self,
            _action: &str,
            _subject: &rime_agent_core::auth::ProcessSubject,
        ) -> Result<rime_agent_core::auth::Verdict, rime_agent_core::auth::AuthError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(rime_agent_core::auth::Verdict::Refused)
        }
    }

    /// A daemon whose only authenticator refuses, and a handle to the count.
    ///
    /// `Daemon::new` hardcodes `PolkitAuthenticator`, so this is a struct
    /// literal — every field is `pub` and the `auth` field's own doc comment
    /// says it is boxed behind the trait "so a test can build a daemon that
    /// never reaches polkit". This is the first test in the repository to
    /// take it up on that.
    fn daemon_that_refuses_every_password() -> (Arc<Daemon>, Arc<std::sync::atomic::AtomicUsize>) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let daemon = Arc::new(Daemon {
            registry: std::sync::Mutex::new(crate::registry::Registry::new()),
            config: std::sync::Mutex::new(rime_agent_core::config::Config::default()),
            grants: crate::grants::GrantAuthority::new(),
            auth: Box::new(CountingRefusal(calls.clone())),
            tests: crate::worktrees::TestObservations::new(),
            challenges: std::sync::Mutex::new(rime_agent_core::webauthn::ChallengeStore::new()),
        });
        (daemon, calls)
    }

    fn asked(calls: &Arc<std::sync::atomic::AtomicUsize>) -> usize {
        calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A caller with a real, live pid, so `ProcessSubject::for_pid` succeeds
    /// and a test that means to reach polkit is not stopped short of it by an
    /// unreadable `/proc` entry.
    fn caller_with_a_peer() -> Caller {
        Caller::new(Some(Peer {
            pid: std::process::id() as libc::pid_t,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        }))
    }

    #[test]
    fn a_verified_key_authorises_a_remote_grant_and_polkit_is_never_asked() {
        // The whole point of commit 5, and the defect it repairs is one that
        // passes every unit test of `decide_origin`: the gate returns
        // `Ok(origin)` correctly and the caller then dies one line later in
        // `authenticate`, because `org.rimeos.agent.policy` gives both actions
        // `allow_any: no` and says in its own comment that "there is no
        // password that makes a remote caller local".
        //
        // So this asserts a call that must NOT happen. The authenticator
        // refuses everything: if polkit were still consulted, the grant would
        // come back `NotAuthenticated` rather than `Ok`.
        let signer = Signer::new("the owner's key");
        for origin in remote_origins() {
            for (scope, kind, ttl_ms) in [
                (None, GrantKind::SystemAccess, 900_000u64),
                (Some(7), GrantKind::BreakGlass, 60_000),
            ] {
                let (daemon, calls) = daemon_that_refuses_every_password();
                let s = submitted();
                let factor = receipt(&signer, scope, kind, ttl_ms, UP_UV);
                let e = Elevating {
                    policy: OriginPolicy::RemoteElevationAllowed,
                    scope,
                    ttl_ms,
                    factor: Some(&s),
                };
                let (got, proof) = authorise_grant_with(
                    &daemon,
                    &unsessioned(origin),
                    &caller_with_a_peer(),
                    kind,
                    "ask",
                    &e,
                    move |_| Ok(factor),
                )
                .expect("a verified key is the authentication for this column");
                assert_eq!(got, origin);
                // And the audit line says which of the two happened, so a
                // touch collected over a network is not recorded as a password
                // typed at this keyboard.
                assert_eq!(proof.recorded_as(kind), "security-key:the owner's key");
                assert_eq!(
                    asked(&calls),
                    0,
                    "polkit was consulted for a {origin} caller, which is the wall this \
                     commit exists to remove"
                );
            }
        }
    }

    #[test]
    fn the_local_column_still_goes_to_polkit_and_is_still_refused_when_it_says_no() {
        // The negative control, and it is what keeps the test above from
        // passing for the wrong reason. If `authorise_grant` had simply
        // stopped calling `authenticate`, the assertion above would be green
        // and every local elevation on the machine would have become
        // passwordless. Same daemon, same refusing authenticator, local
        // origin: refused, and asked exactly once.
        for origin in RequestOrigin::ALL.iter().copied().filter(|o| o.is_local()) {
            let (daemon, calls) = daemon_that_refuses_every_password();
            let e = Elevating {
                policy: OriginPolicy::RemoteElevationAllowed,
                scope: None,
                ttl_ms: 900_000,
                factor: None,
            };
            let err = authorise_grant_with(
                &daemon,
                &unsessioned(origin),
                &caller_with_a_peer(),
                GrantKind::SystemAccess,
                "ask",
                &e,
                never_redeemed,
            )
            .expect_err("the local column is polkit's, and polkit said no");
            assert!(matches!(err, GrantError::NotAuthenticated(_)), "{origin}: {err}");
            assert_eq!(asked(&calls), 1, "{origin}");
        }
    }

    #[test]
    fn a_key_with_no_pin_cannot_approve_root_from_anywhere_remote() {
        // Both `GrantKind`s are root — §4.4 is capability-scoped root and §4.5
        // is break-glass — so a bare touch is not enough and the refusal has
        // to name `fido2-token -S`. The signature here is perfectly good; only
        // the UV bit inside the signed bytes differs.
        let signer = Signer::new("a key with no pin");
        for kind in [GrantKind::SystemAccess, GrantKind::BreakGlass] {
            let s = submitted();
            let factor = receipt(&signer, None, kind, 900_000, UP);
            let e = Elevating {
                policy: OriginPolicy::RemoteElevationAllowed,
                scope: None,
                ttl_ms: 900_000,
                factor: Some(&s),
            };
            let err = decide_origin(
                &unsessioned(RequestOrigin::RemoteControl),
                "ask",
                kind,
                &e,
                move |_| Ok(factor),
            )
            .expect_err("a touch without a pin is not enough for root");
            assert!(
                matches!(
                    err,
                    GrantError::RemoteElevation(RemoteElevationRefused::NotUserVerified { .. })
                ),
                "{kind:?}: {err}"
            );
        }
    }
}
