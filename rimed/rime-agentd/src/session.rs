//! Starting sessions, reading their terminals, and attaching to them.

use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use rime_agent_core::adapter;
use rime_agent_core::checkpoint;
use rime_agent_core::hook;
use rime_agent_core::mcpconf;
use rime_agent_core::client::SESSION_ENV;
use rime_agent_core::paths;
use rime_agent_core::pluginconf;
use rime_agent_core::config;
use rime_agent_core::destination::{Allowlist, Destination};
use rime_agent_core::origin::OriginSource;
use rime_agent_core::policy::{NetworkPolicy, PolicyError, RequestOrigin};
use rime_agent_core::profile;
use rime_agent_core::project;
use rime_secret_core::identity;
use rime_agent_core::protocol::{AgentState, ErrorKind, Response, RunRequest, SessionInfo};
use rime_agent_core::sandbox::{self, EgressBridge, SandboxError, SandboxSpec, BRIDGE_PORT};
use rime_agent_core::session as logic;
use rime_agent_core::term::WinSize;

use crate::egress;
use crate::privilege::Caller;
use crate::pty;
use crate::registry::{self, now_secs, Handle};
use crate::Daemon;

/// How long the reader thread waits for output before re-evaluating state.
///
/// One second: fast enough that the idle transition lands on time, slow enough
/// that an idle session costs one wakeup per second rather than a spin.
const POLL_INTERVAL_MS: i32 = 1000;

/// Start a session.
///
/// `peer` is the connection's credentials, and it is what establishes the
/// session's origin (§7). It is passed rather than looked up for the same
/// reason the privilege verbs take it: the origin has to come from the
/// kernel's view of who connected, and a handler that could reach for the
/// request instead would eventually do so.
pub fn start(daemon: &Arc<Daemon>, req: RunRequest, caller: &Caller) -> Result<SessionInfo> {
    let cwd = PathBuf::from(&req.cwd);
    if !cwd.is_absolute() {
        bail!("working directory {} must be absolute", cwd.display());
    }
    if !cwd.is_dir() {
        bail!("working directory {} does not exist", cwd.display());
    }

    // Read fresh, not the copy the daemon loaded at startup. `rime agent
    // default codex` writes agent.json and returns; with the cached copy every
    // later `a` still started the agent the daemon booted with, until the
    // daemon was restarted (L16, 2026-09-24: default set to codex, `a` started
    // claude). The rest of this function already reads the file fresh (below),
    // so the default agent was the one setting that lagged. The cache is kept
    // current for the readers that still use it.
    let cfg = config::Config::load();
    *daemon.config.lock().expect("config lock") = cfg.clone();

    // ── §36's `[identity.agent]`, before anything is resolved ──────────────
    //
    // P2-013. Which assistant runs here is the project's to say, and this is
    // the only place in the build that decides it — so it is the only place
    // the binding can be enforced.
    //
    // The root is the PROJECT's, not `req.cwd`. A check keyed on the working
    // directory would be stepped around by `cd src && rime agent run --agent
    // other`, which is not a hypothetical: a session's own cwd is the first
    // thing an agent can change. `project::detect` is the same git-toplevel
    // rule `rime secret` grants are keyed on, so the binding and the grant
    // describe the same project.
    //
    // A directory that is not in a repository has no project record anywhere
    // in this build, and rather than ignore an `rime.toml` sitting in it, the
    // directory itself is used. That errs towards enforcing.
    let project_root = project::detect(&cwd)
        .map(|p| PathBuf::from(p.root))
        .unwrap_or_else(|| cwd.clone());
    // The owner is this daemon's own uid and not the peer's: the session will
    // run as this user, `rime-agentd` is a user service, and an owner taken
    // from the request would be the request deciding whose file counts.
    //
    // Safe: getuid cannot fail.
    let owner_uid = unsafe { libc::getuid() };
    let owner_name = std::env::var("USER").unwrap_or_else(|_| format!("uid {owner_uid}"));
    // A project with no `rime.toml` binds nothing. **Every other failure
    // refuses**: a file that could not be read is not a file that says
    // nothing, and starting the user's default agent because `rime.toml` was
    // unreadable is how a session would unbind the project it is starting in.
    let identities =
        identity::Identities::read_or_unbound(&project_root, owner_uid, &owner_name).map_err(
            |e| {
                IdentityRefused(format!(
                    "this project's rime.toml could not be read, so which agent \
                     it binds is unknown, and a session will not be started \
                     under an assistant this build cannot check: {e}"
                ))
            },
        )?;

    let bound_agent = identities.agent.as_ref();
    let agent_id = match (req.agent.as_deref(), bound_agent) {
        // Named, and the project binds one: §36's refusal.
        (Some(requested), Some(bound)) => {
            bound
                .check(requested)
                .map_err(|e| IdentityRefused(e.to_string()))?;
            requested.to_string()
        }
        // Named nothing: the binding DISPLACES the user's own default. This
        // half is the difference between a binding and a suggestion.
        (None, Some(bound)) => bound.default.clone(),
        (Some(requested), None) => requested.to_string(),
        (None, None) => cfg.default_agent.clone(),
    };
    let adapter = adapter::by_id(&agent_id).ok_or_else(|| {
        // Which of the two said it matters, because the remedies differ: one
        // is a typo on a command line and the other is a line in a file.
        match bound_agent {
            Some(bound) if bound.default == agent_id => anyhow::Error::new(IdentityRefused(
                format!(
                    "this project binds [identity.agent] default = \"{}\", and \
                     this build has no agent adapter by that name. The binding \
                     is refused rather than ignored — falling back to the \
                     user's default would start a session under an assistant \
                     the project did not name. known agents: {}",
                    bound.default.escape_debug(),
                    adapter::ids().join(", ")
                ),
            )),
            _ => anyhow::anyhow!("no agent adapter named {agent_id:?}"),
        }
    })?;

    // The generic adapter carries no program of its own, so the caller has to
    // supply one; anything else would be a session with nothing to run.
    let explicit = req.args.first().filter(|_| adapter.id == "generic");
    let program = adapter
        .resolve_program(explicit.map(|s| s.as_str()))
        .with_context(|| {
            format!("the {agent_id} adapter needs a program to run; pass one after `--`")
        })?;
    let extra: Vec<String> = if explicit.is_some() {
        req.args[1..].to_vec()
    } else {
        req.args.clone()
    };

    if pty::resolve_program(&program).is_none() {
        bail!(
            "{program} is not installed or not on PATH.\n{}",
            install_hint(&program)
        );
    }

    // Resolve the permission dimensions before anything is created.
    //
    // Normalised first, so the record and the enforcement agree about what the
    // session has — a `strict` request carries the client's default `open`
    // network, and storing that would list an isolated session as networked.
    // Validated second, so a client that skipped its own checks, or one built
    // against a newer vocabulary, is refused here rather than granted a
    // dimension nothing in this build enforces.
    let policy = req.policy.normalised();
    // The allowlist is read fresh rather than taken from the daemon's cached
    // configuration. A destination policy that only changed on a daemon
    // restart is one people widen once and never narrow again, and this is the
    // daemon reading its own user's file — nothing the session can write.
    let runtime_config = config::Config::load();
    let allowlist = runtime_config.allowlist();
    policy
        .validate_for(
            &allowlist,
            &runtime_config.connector_allow,
            &runtime_config.plugin_allow,
        )
        .map_err(PolicyRefused)?;

    // P2-012: this session may reach fewer destinations than the runtime does.
    //
    // Validated against the runtime's list above and narrowed out of it here,
    // in that order, because the two questions are different: `validate_for`
    // refuses an allowlisted session whose RUNTIME list is empty — nobody has
    // filled the policy in — and `session_allowlist` refuses a session that
    // named a destination the runtime does not cover. Collapsing them would
    // answer the second question with the first one's message.
    //
    // From here down `allowlist` is the SESSION's, and every use of it — the
    // egress proxy, the recorded confinement §6.2 judges tool calls against,
    // the line `rime agent status` prints — is the narrowed one. One binding
    // rather than two, so a later reader cannot pick the wrong one.
    let allowlist =
        session_allowlist(&allowlist, req.allow.as_deref(), policy.effective_network())?;

    // Dimension 1 is the agent's own, and only the adapter knows whether this
    // one can express it. Refused rather than dropped: a `--agent-bypass` that
    // silently did nothing would leave the user believing confirmations were
    // off.
    if let Some(why) = adapter.refuses_native_mode(policy.native, is_root()) {
        bail!("{why}");
    }

    // Fail closed before anything is created: a session must never start with
    // weaker confinement than was asked for.
    sandbox::preflight(policy.sandbox).map_err(SandboxRefused)?;

    // Dimension 6's companion: where this session will be driven from.
    //
    // Established here, once, from the connection that asked for it — and
    // never afterwards from anything the session says, because a session that
    // could name its own origin could name the local one. `--origin` on the
    // command line is a DECLARATION and is checked against the observation
    // before it is accepted; a local origin is refused whatever is sent.
    //
    // Refused rather than defaulted when it cannot be established: the default
    // is `local-terminal`, which is what §7 reserves root for.
    let who = crate::privilege::origin(daemon, caller);
    let session_origin = crate::privilege::for_new_session(&who, req.request_origin)
        .map_err(OriginRefused)?;

    // ── dimension 3: the grant, before anything exists to clean up ─────────
    //
    // §3.3: root is delegated, not inherited. A session that asks for either
    // elevated mode gets one only after a human at this machine has said so,
    // and the whole of that decision happens here — before the worktree, the
    // checkpoint, the reserved id and the PTY, so a refused password leaves
    // nothing behind and a refused ORIGIN never reaches the password at all.
    //
    // The order inside `authorise_grant` is the security property; it is
    // written out there. The TTL is checked first, so a typo in `--ttl` fails
    // in front of the user instead of after a password dialog they then find
    // out was pointless.
    // §P1-037. Checked here, with the TTL, and for the same reason: a caller
    // who asked for two mechanisms that cannot both apply should find that out
    // in front of their own terminal, not after a capsule has been created.
    crate::disposable::check(
        req.disposable,
        policy.sandbox.is_confined(),
        req.copy_out.as_deref(),
        req.worktree.is_some(),
        req.checkpoint,
    )?;
    // P2-012 gap 5, checked here with the pair above and for the same reason:
    // the half of `trust_ca` that can be decided from the request alone costs
    // nothing and belongs before anything exists to clean up. The file's
    // CONTENTS are checked later, against the copy the capsule will actually
    // read — see `browser_ca`'s own note on why that order is the one that
    // means something.
    crate::browser_ca::check(req.trust_ca.as_deref(), policy.sandbox.is_confined())?;
    // P2-012 route B, the half that is a fact about the request. The pin — and
    // therefore whether the narrowed allowlist is the one destination this
    // credential may be spent at — needs the secret service and is checked
    // below, where the answer can be turned into a certificate.
    present_check(
        req.present.as_deref(),
        req.trust_ca.as_deref(),
        policy.sandbox.is_confined(),
        policy.effective_network(),
    )?;
    // §1.4. The same check the CLI already made, made again here because a
    // client is not a boundary — and made HERE, with the other facts about the
    // request, so a refused name leaves no reserved id, worktree or grant
    // behind. A `bad_request` with the validator's own sentence.
    let session_name = rime_agent_core::session::session_name_opt(req.name.as_deref())
        .map_err(|why| anyhow!("{why}"))?;

    let wanted_grant = policy.needs_grant();
    if wanted_grant.is_none() && req.ttl_ms.is_some() {
        // A `--ttl` with nothing to bound is a user who believes they asked
        // for something they did not. Refused rather than ignored.
        bail!(
            "--ttl bounds a system-access grant, and this session is not asking for one; add \
             `--system-access session` or `--unsafe-everything`, or drop the --ttl"
        );
    }
    if wanted_grant.is_none() && req.capabilities.is_some() {
        // The same refusal as a `--ttl` with nothing to bound, and for the
        // same reason: a caller who narrowed a grant they did not ask for
        // believes they asked for something they did not.
        bail!(
            "--capabilities narrows a system-access grant, and this session is not asking for \
             one; add `--system-access session`, or drop the --capabilities"
        );
    }
    let authorised = match wanted_grant {
        None => None,
        Some(kind) => {
            let ttl_ms = rime_agent_core::grant::ttl_for(kind, req.ttl_ms)
                .map_err(|e| TtlRefused(e.to_string()))?;
            // Ahead of `authorise_grant` for the reason the TTL is: a typo in
            // `--capabilities` should fail in front of the person who typed
            // it, not after a password dialog they then find out was
            // pointless.
            let capabilities =
                rime_agent_core::grant::capabilities_for(kind, req.capabilities.as_deref())
                    .map_err(|e| CapabilitiesRefused(e.to_string()))?;
            let (grant_origin, proof) = crate::privilege::authorise_grant(
                daemon,
                &who,
                caller,
                kind,
                "ask for a system-access grant",
                // §7's second column, for the session being started (P0-014).
                //
                // The policy is THIS request's dimension 6 — normalised and
                // validated above — and not the daemon's configured default,
                // which `Config::policy()` only assembles for a session
                // started without the flags. A session started with
                // `--origin-policy remote` overrides it, and reading the
                // config here would mean the per-session dimension governs
                // nothing.
                //
                // `scope: None` is not an oversight: this call is deliberately
                // ahead of `registry.allocate()`, so that a refused password
                // leaves no reserved id behind, and there is therefore no id
                // for the key to have signed over. `Challenge.session` is
                // `Option<u32>` for exactly this caller.
                &crate::privilege::Elevating {
                    policy: policy.origin,
                    scope: None,
                    ttl_ms,
                    factor: req.second_factor.as_ref(),
                },
            )
            .map_err(|e| GrantRefused(e.to_string()))?;
            Some((kind, ttl_ms, capabilities, grant_origin, proof))
        }
    };

    // Resolve the project, then the worktree, then the working directory. Each
    // step can change where the session actually runs.
    let detected = project::detect(&cwd);
    let mut workdir = cwd.clone();
    let mut worktree_name = None;

    if let Some(name) = req.worktree.as_deref() {
        let proj = detected
            .as_ref()
            .with_context(|| format!("{} is not in a git repository, so --worktree cannot be used", cwd.display()))?;
        workdir = project::ensure_worktree(proj, name)
            .with_context(|| format!("creating the worktree {name}"))?;
        worktree_name = Some(project::Project::worktree_branch(proj, name));
    }

    if let Some(proj) = detected.as_ref() {
        // Reported, not swallowed. `let _ =` here is how a bug in
        // project::remember stayed invisible for as long as it did: every real
        // project slug contained slashes, remember failed on the missing
        // parent directories for all of them, and nothing ever said so — so
        // `rime project list` was simply always empty. Failing to remember a
        // project must not stop a session from starting, but it must be
        // audible.
        if let Err(e) = project::remember(proj) {
            eprintln!("rime-agentd: could not record project {}: {e:#}", proj.name);
        }
    }

    // The checkpoint is taken against the directory the agent will actually
    // work in, which for a worktree run is the worktree, not the main tree.
    let checkpoint_id = if req.checkpoint || cfg.auto_checkpoint {
        match checkpoint::create(&workdir, "before agent task", None) {
            Ok(cp) => Some(cp.id),
            Err(e) => {
                // A project without git, or a git failure, must not stop the
                // agent from running — but the user has to be told the undo
                // they asked for does not exist.
                eprintln!("rime-agentd: checkpoint skipped: {e:#}");
                None
            }
        }
    } else {
        None
    };

    // Reserved on disk, not merely counted in memory: an id that collides with
    // a record left by an earlier daemon overwrites that session's transcript.
    // Held as a guard so a failure between here and the spawn gives the id back
    // instead of leaving an empty record behind.
    let reservation = daemon.registry.lock().expect("registry lock").allocate()?;
    let id = reservation.id();
    // Not best-effort: the sandbox binds this path read-write and sets TMPDIR
    // to it. If it cannot be created, or cannot be made private, the session
    // would start with an unexpected scratch directory and fail later in a much
    // harder place to diagnose.
    //
    // `ensure_scratch_dir`, not `ensure_private_dir(&scratch_dir(id))`: the
    // root lives in world-writable /tmp and is a boundary this account has to
    // own, and ensuring only the leaf leaves a root another account pre-created
    // in place while reporting success. See `paths::SCRATCH_ROOT_PREFIX` for
    // the measurement.
    let scratch = paths::ensure_scratch_dir(id).with_context(|| {
        format!(
            "preparing the session scratch directory {}",
            paths::scratch_dir(id).display()
        )
    })?;

    // §6.1: the settings document that subscribes Claude to its own lifecycle,
    // written into the scratch directory the sandbox already binds. Best-effort
    // by design — a session whose hooks could not be installed reports its
    // state from the PTY scanner, which is the fallback §6.1 keeps and not a
    // reason to refuse to start. `hook_settings` says what went wrong, once.
    let hook_settings = install_hook_settings(
        adapter,
        &scratch,
        detected.as_ref().map(|p| std::path::Path::new(&p.root)),
        &policy,
        &runtime_config.plugin_allow,
    );

    // §12: the shim's directory goes first on the session's PATH, so a skill's
    // own `git push` reaches the broker without the skill knowing there is one.
    // Only for a confined session — an unconfined one has the user's own git,
    // the user's own credential helper, and no reason to be routed anywhere.
    let session_bin = policy
        .sandbox
        .is_confined()
        .then(|| install_git_shim(&scratch))
        .flatten();
    // §10.2 and P1-026/P1-028: the connectors this session gets, decided by the
    // runtime and handed over as a file, rather than whatever the agent finds
    // on the machine. Best-effort in the same sense the hook settings are — a
    // session whose configuration could not be written starts with the
    // connectors it would have had — but not silently: `install_mcp_config`
    // says what went wrong and the session record says the configuration is
    // absent, so nothing downstream reports a confinement that did not happen.
    let mcp_config = install_mcp_config(
        adapter,
        &scratch,
        &workdir,
        &policy,
        &runtime_config.connector_allow,
    );

    let mut extra = extra;
    if let Some(path) = mcp_config.as_ref() {
        let mut with_mcp = adapter.mcp_config_args(path);
        with_mcp.append(&mut extra);
        extra = with_mcp;
    }
    if let Some(path) = hook_settings.as_ref() {
        let mut with_hooks = adapter.hook_settings_args(path);
        with_hooks.append(&mut extra);
        extra = with_hooks;
    }

    let args = adapter.build_args(policy.native, req.prompt.as_deref(), &extra);
    let size = WinSize {
        cols: req.cols,
        rows: req.rows,
    }
    .or_fallback();

    // Build the sandbox.
    let mut spec = SandboxSpec::new(policy, paths::home(), paths::runtime_dir());
    // /run is masked, which takes the resolv.conf symlink target with it. Bind
    // the target back or the session has no DNS at all.
    spec.run_ro = sandbox::resolv_binds();
    spec.control_socket = paths::control_socket();
    spec.scratch = scratch.clone();
    spec.cwd = workdir.clone();
    spec.rw.push(workdir.clone());
    if let Some(proj) = detected.as_ref() {
        // The main checkout as well as the worktree: a worktree's `.git` file
        // points into the main repository, so a worktree session that cannot
        // reach it cannot run git at all.
        let root = PathBuf::from(&proj.root);
        if !spec.rw.contains(&root) {
            spec.rw.push(root);
        }
    }
    adapter.apply_sandbox(&mut spec);
    // Read-only, and that is the one part of this bridge an agent cannot undo.
    // `build_argv` applies the read-only allowlist after the scratch bind, so
    // this lands on top of a directory the session can otherwise write: the
    // hook subscriptions are fixed at spawn. It does not make the hook
    // authoritative — `--bare`, a nested agent and `disableAllHooks` in the
    // agent's own writable `~/.claude` all still silence it — which is why
    // nothing downstream is allowed to depend on the hook having run.
    if let Some(path) = hook_settings.as_ref() {
        spec.ro.push(path.clone());
    }
    // Read-only for the reason the hook settings are, and here it is the whole
    // point rather than tidiness: the scratch is bound writable, so a curated
    // MCP configuration the session could rewrite is one it could put its own
    // unwrapped definitions back into — which is the hole this closes.
    if let Some(path) = mcp_config.as_ref() {
        spec.ro.push(path.clone());
    }
    // Read-only for the same reason the hook settings are: the scratch is
    // bound writable, and a shim the session could rewrite is one it could
    // point at something else. It holds no credential either way — this is
    // tidiness, not a boundary.
    if let Some(bin) = session_bin.as_ref() {
        spec.ro.push(bin.clone());
    }

    // P0-003's first criterion, enforced at spawn rather than left to whether
    // somebody has run the migration yet. `settings.json` is bound read-only
    // above; this puts a copy of it, with the credential values gone, on top.
    if let Some((from, at)) = install_redacted_settings(adapter, &scratch, &spec.home) {
        // The copy itself goes on the read-only list too, for the reason the
        // hook settings do: the scratch directory is bound writable, so a copy
        // that were writable through its own path would be one the session
        // could edit — and both files should have the same story.
        spec.ro.push(from.clone());
        spec.ro_at.push((from, at));
    }

    // P2-012's gap 5: one private CA, trusted by this session's browser and by
    // nothing else on the machine.
    //
    // `?` and not a warning, which is the difference between this and every
    // other installer around it. A session that did not get its redacted
    // settings still runs; a capsule that asked to trust a CA and did not get
    // one does not, and it fails by sitting silently on a refused handshake
    // until `rime browser --timeout` stops it. The refusal has to arrive
    // before the browser starts or it does not arrive at all.
    //
    // Both files go on the read-only list for the reason the redacted copy
    // does — the scratch is bound writable — and here it is a boundary rather
    // than tidiness: a policy document the session could rewrite is one it
    // could point at a CA of its own, and the whole claim being made is that
    // the capsule trusts exactly the root the caller named.
    //
    // Route B mints its own CA here rather than taking one from the caller,
    // and the two are refused together above: a capsule with `--present` is
    // pinned to one destination and this daemon terminates it, so a
    // caller-supplied root would be a root for a connection that no longer
    // exists. One `install` call either way, because it binds one policy
    // document over one path and a second call would bind over its own bind.
    let mut intercept: Option<Arc<crate::intercept::Intercept>> = None;
    let trust_ca: Option<String> = match (req.trust_ca.as_deref(), req.present.as_deref()) {
        (Some(ca), _) => Some(ca.to_string()),
        (None, Some(service)) => {
            let minted = crate::intercept::mint(
                &scratch,
                &present_pin(service, &allowlist)?,
                present_record(
                    service,
                    &project_root,
                    id,
                    session_origin.origin,
                    session_origin.source,
                ),
                rime_secret_core::paths::socket(),
            )
            .map_err(|e| anyhow!("{e}"))?;
            let ca = minted.ca.to_string_lossy().into_owned();
            intercept = Some(Arc::new(minted.intercept));
            Some(ca)
        }
        (None, None) => None,
    };
    if let Some(ca) = trust_ca.as_deref() {
        let installed =
            crate::browser_ca::install(ca, &scratch, Path::new(crate::browser_ca::FIREFOX_POLICY))?;
        spec.ro.push(installed.ca);
        spec.ro.push(installed.policy.clone());
        spec.ro_at.push((installed.policy, installed.at));
    }

    // The profile's writable directories have to exist before the sandbox binds
    // them: bwrap binds with `-try`, and a `-try` for a path that is not there
    // is a no-op, so the entry would resolve inside the tmpfs that masks $HOME.
    // The agent would write its transcripts and its trusted-directory list into
    // memory and lose both at exit — which reads as the agent forgetting, not
    // as a sandbox that dropped a mount. Not fatal: a session with a
    // session-local plugin cache still runs, and refusing to start over a
    // directory nobody has needed yet would be worse.
    if spec.policy.sandbox.is_confined() {
        if let Some(profile) = adapter.profile() {
            match profile.prepare(&spec.home) {
                Ok(made) if !made.is_empty() => eprintln!(
                    "rime-agentd: created {} missing {} profile director{}",
                    made.len(),
                    adapter.id,
                    if made.len() == 1 { "y" } else { "ies" }
                ),
                Ok(_) => {}
                Err(e) => eprintln!(
                    "rime-agentd: could not prepare the {} profile ({e}); \
                     anything it writes below a missing directory stays in the session",
                    adapter.id
                ),
            }
        }
    }

    // The allowlist's only route out. Started before the session, so an agent
    // that resolves a proxy on its first line finds one there; and inside the
    // scratch directory, which is already bound read-write, so it needs no
    // mount of its own and cannot disturb the ordering the `/run` mask
    // depends on. `finish` deletes that directory and the proxy stops with it.
    //
    // The `?` is the fail-closed half: a session that asked for an allowlist
    // and whose proxy did not start does not run with the host's network, and
    // does not run at all.
    if policy.effective_network() == NetworkPolicy::Allowlist {
        let program = bridge_program()?;
        let socket = scratch.join("egress.sock");
        egress::start(id, &socket, allowlist.clone(), intercept.clone())
            .with_context(|| format!("starting the egress proxy for session {id}"))?;
        spec.egress = Some(EgressBridge {
            program,
            socket,
            port: BRIDGE_PORT,
        });
    }

    spec.env_set.push(("HOME".into(), paths::home().to_string_lossy().into_owned()));
    spec.env_set.push(("PWD".into(), workdir.to_string_lossy().into_owned()));
    let path = match session_bin.as_ref() {
        Some(bin) => format!("{}:{}", bin.display(), inherited_path()),
        None => inherited_path(),
    };
    spec.env_set.push(("PATH".into(), path));
    spec.env_set.push((SESSION_ENV.into(), id.to_string()));
    spec.env_set
        .push(("RIME_AGENT_SANDBOX".into(), policy.sandbox.to_string()));
    // The other five dimensions a session may usefully know about itself. A
    // hook that wants to say "this session has no network" reads this rather
    // than guessing from the sandbox name, which stopped being the authority
    // on the network when the dimensions were split.
    spec.env_set
        .push(("RIME_AGENT_NATIVE_MODE".into(), policy.native.to_string()));
    spec.env_set
        .push(("RIME_AGENT_NETWORK".into(), policy.effective_network().to_string()));
    spec.env_set
        .push(("TMPDIR".into(), scratch.to_string_lossy().into_owned()));
    // Where the control socket is, because a session that cannot name its
    // runtime directory cannot find the socket it was just handed. Resolved
    // here rather than read later, so the name the session is given and the
    // path the socket is bound at cannot be two different strings.
    //
    // Without this a confined session gets `--clearenv` and nothing to replace
    // it, so `paths::runtime_dir` falls back to `/run/user/<uid>` — right on an
    // ordinary login and wrong for any daemon with an `XDG_RUNTIME_DIR` of its
    // own, where every `rime agent event` and every hook reports "the agent
    // runtime is not running" from inside a session the runtime is
    // demonstrably running.
    //
    // Before `req.env`, with the other variables the daemon owns: `resolved_env`
    // is first-seen-wins, and a caller redirecting a session's reporting at
    // another socket is not something a `--env` flag should be able to do.
    spec.env_set.push((
        "XDG_RUNTIME_DIR".into(),
        sandbox::real_target(&spec.runtime_dir)
            .to_string_lossy()
            .into_owned(),
    ));
    for (k, v) in &req.env {
        spec.env_set.push((k.clone(), v.clone()));
    }
    for name in ["USER", "LOGNAME", "SHELL"] {
        if let Ok(val) = std::env::var(name) {
            spec.env_set.push((name.to_string(), val));
        }
    }
    // After `req.env`, so a caller's own `WAYLAND_DISPLAY` still wins.
    if let Some(pair) = session_display(policy.sandbox.is_confined(), req.disposable, || {
        crate::clipboard::display().map(|seat| seat.display)
    }) {
        spec.env_set.push(pair);
    }

    // bwrap will not mount on a path that traverses a symlink ("Can't mount on
    // symlink destination"), and an atomic OS reaches every home through one:
    // /root -> var/roothome, /home -> var/home. So the tmpfs that masks $HOME,
    // and any bind under a symlinked home, abort the session unless the target
    // is resolved to its real path first. The logical paths still exist inside
    // the sandbox as symlinks to the resolved ones, so $HOME and the working
    // directory keep resolving. cwd is deliberately not resolved: it becomes a
    // --chdir, which follows symlinks, not a mount point.
    spec.home = sandbox::real_target(&spec.home);
    spec.runtime_dir = sandbox::real_target(&spec.runtime_dir);
    if !spec.scratch.as_os_str().is_empty() {
        spec.scratch = sandbox::real_target(&spec.scratch);
    }
    if !spec.control_socket.as_os_str().is_empty() {
        spec.control_socket = sandbox::real_target(&spec.control_socket);
    }
    if let Some(bridge) = spec.egress.as_mut() {
        // Resolved for the same reason as the scratch directory it sits in:
        // the bridge connects to this path from inside the sandbox, where the
        // bind was made against the real one.
        bridge.socket = sandbox::real_target(&bridge.socket);
    }
    for p in spec.rw.iter_mut() {
        *p = sandbox::real_target(p);
    }
    for p in spec.ro.iter_mut() {
        *p = sandbox::real_target(p);
    }
    for p in spec.mask.iter_mut() {
        *p = sandbox::real_target(p);
    }
    for (from, at) in spec.ro_at.iter_mut() {
        *from = sandbox::real_target(from);
        *at = sandbox::real_target(at);
    }

    // The grant is minted now that the session has an id to be bound to, and
    // before the process starts: §3.3 wants the grant "bound to a concrete
    // agent session", and a grant issued after the agent was already running
    // would have a window in which the session existed and the record did not.
    let issued = authorised.map(|(kind, ttl_ms, capabilities, grant_origin, proof)| {
        daemon.grants.issue(
            proof,
            kind,
            id,
            adapter.id,
            detected.as_ref().map(|p| p.root.as_str()),
            ttl_ms,
            capabilities,
            grant_origin,
            rime_agent_core::request::now_ms(),
        )
    });

    // §P1-037. A disposable session's PTY child is the disposable ENGINE, and
    // the adapter runs inside the capsule it creates. `build_argv` is not in
    // that path at all: the policy is `unrestricted` here — `disposable::
    // check` refused any other above — so bwrap would add nothing, and if it
    // were added it would confine the container client rather than the agent.
    //
    // The engine's own EXIT/INT/TERM traps are what remove the environment,
    // so there is no teardown here to get wrong: killing this child tears the
    // capsule down, which is exactly the behaviour `rime agent kill` should
    // have.
    let capsule = req.disposable.then(|| crate::disposable::name_for(id));
    let argv = if req.disposable {
        // The engine's own overrides, set EXPLICITLY rather than relied on to
        // arrive by inheritance. They decide which directory it removes
        // recursively and which program it drives, and the inheritance that
        // carries them today is a bug elsewhere that a correct fix would take
        // away — see `disposable::engine_env`.
        for pair in crate::disposable::engine_env(|n| std::env::var(n).ok()) {
            spec.env_set.push(pair);
        }
        crate::disposable::argv(id, &workdir, req.copy_out.as_deref(), &program, &args)?
    } else {
        sandbox::build_argv(&spec, &program, &args).map_err(SandboxRefused)?
    };
    // §P2-011. A pure exec-chain prefix when a budget is configured, and the
    // identity function when one is not — which is the default, so an
    // unbudgeted session's argv is byte-identical to what it was before this
    // line existed. `spec.runtime_dir` and not the daemon's own: the child's
    // `XDG_RUNTIME_DIR` is what decides whether systemd-run can reach a user
    // manager, and they are not always the same directory.
    let argv = crate::budget::wrap(argv, id, req.disposable, &spec.runtime_dir, &cfg)
        .map_err(BudgetRefused)?;
    let env = sandbox::resolved_env(&spec);

    // A confined session gets its environment from bwrap's --setenv, so the
    // process environment is only used for the unconfined path.
    let spawned = pty::spawn(&argv, &workdir, &env, true, policy.no_new_privs(), size)
        .with_context(|| format!("starting {program}"))?;

    let info = SessionInfo {
        id,
        agent: adapter.id.to_string(),
        program: program.clone(),
        args: args.clone(),
        cwd: workdir.to_string_lossy().into_owned(),
        project: detected.as_ref().map(|p| p.root.clone()),
        project_name: detected.as_ref().map(|p| p.name.clone()),
        name: session_name,
        // Nothing has been printed yet. The output scanner fills this in from
        // the agent's first OSC 0 / OSC 2.
        title: None,
        worktree: worktree_name,
        state: AgentState::Starting,
        detail: None,
        paused: false,
        policy,
        // Read off the binding the egress proxy and the §6.2 confinement are
        // built from, not off `req.allow`, so the three cannot disagree. Shown
        // only when there is a boundary to show: an `open` session's list
        // would be the runtime's, which says nothing about this session.
        allowlist: match policy.effective_network() {
            NetworkPolicy::Allowlist => Some(allowlist.lines()),
            _ => None,
        },
        request_origin: Some(session_origin.origin),
        origin_source: Some(session_origin.source),
        // Which remote device asked for this session, when the connection
        // that asked named one. Carried from the connection rather than from
        // the `Run` request: a session that could name its own actor could
        // name somebody else's phone.
        actor: who.actor.clone(),
        capsule: capsule.clone(),
        grant: issued.as_ref().map(|g| g.id),
        grant_expires_ms: issued.as_ref().map(|g| g.expires_ms),
        // Nothing has been heard from the agent yet. Claude fills this in on
        // its first hook event; an agent that never publishes one leaves it
        // absent, which reads as "not reported" rather than as a mode.
        native_observed: None,
        // Empty, not absent: this daemon has the graph, and a session that has
        // delegated nothing yet must be distinguishable from one whose runtime
        // cannot tell. See `SessionInfo::children`.
        telemetry: None,
        children: Vec::new(),
        pid: spawned.pid,
        started: now_secs(),
        last_activity: now_secs(),
        exit_code: None,
        exit_signal: None,
        attached: 0,
        checkpoint: checkpoint_id,
        cols: size.cols,
        rows: size.rows,
        injected: 0,
    };

    let handle = {
        let mut reg = daemon.registry.lock().expect("registry lock");
        reg.insert(info.clone(), spawned.master, spawned.pid, spawned.pgid)
    };
    // The spec as built, not as it could be rebuilt later: §6.2 must judge a
    // tool call against the confinement the session is actually running under.
    {
        let mut s = handle.lock().expect("session lock");
        s.confinement = Some(Box::new(registry::Confinement { spec, allowlist }));
        // §1.7: the terminal this was started from is the first size it has
        // for its kind of viewer. For a session started on the desktop that
        // is what a phone opening it before anybody attached gives back.
        s.sizes
            .claim(crate::privilege::viewer(&who), size);
    }
    registry::write_record(&info);
    // The session owns its record now, so the id stops being a reservation.
    reservation.commit();
    spawn_reader(Arc::clone(daemon), handle, id);

    Ok(info)
}

/// Write a credential-free copy of the agent's settings file, and say where it
/// goes and what it replaces.
///
/// The file Claude reads for its model, its hooks and its theme is also the
/// file it reads an `env` block from, and that block is applied to every tool
/// the session runs. A PAT put there reaches the session's tools whatever the
/// sandbox does with the environment it started the process in, because it does
/// not arrive through the environment at all. So the copy, and a bind of the
/// copy over the original.
///
/// `None` when there is nothing to strip, which is the common case and the one
/// the machine should end up in permanently once `rime secret migrate` has run.
/// Also `None` when the copy could not be written — best-effort in the same
/// sense the hook bridge is, and for the same reason: it is logged, and a
/// session that would otherwise start is not refused over it. What that costs
/// is stated where it happens.
fn install_redacted_settings(
    adapter: &adapter::Adapter,
    scratch: &Path,
    home: &Path,
) -> Option<(PathBuf, PathBuf)> {
    let profile = adapter.profile()?;
    let entry = profile.entry_for(profile::Base::Root, Path::new(SETTINGS_FILE))?;
    let real = profile.entry_path(home, entry);
    let raw = std::fs::read(&real).ok()?;
    let (redacted, names) = profile::settings_without_credentials(&raw)?;

    let copy = scratch.join(REDACTED_SETTINGS_FILE);
    if let Err(e) = std::fs::write(&copy, redacted) {
        eprintln!(
            "rime-agentd: could not write {} ({e}), so {} starts with {} as it is — \
             {} reach the session's tools",
            copy.display(),
            adapter.id,
            real.display(),
            names.join(", ")
        );
        return None;
    }
    eprintln!(
        "rime-agentd: {} is bound without {} — store credentials with \
         `rime secret add` and remove them from that file with `rime secret migrate`",
        real.display(),
        names.join(", ")
    );
    Some((copy, real))
}

/// Write the `git` a confined session finds first, and say where its directory
/// is.
///
/// §12: the user's existing skills keep using normal tools. A skill runs
/// `git push`, this is what runs, and it asks the broker to perform the push
/// rather than needing a credential of its own. Everything it does not
/// recognise it execs `/usr/bin/git` for.
///
/// A shell script rather than a symlink or a copy: `rime` has to be invoked as
/// `rime git-shim -- …`, and the session's `PATH` entry has to be called `git`.
/// Two lines of `sh` are the whole of it, and being readable matters more here
/// than being clever — the agent can read this file, and what it says is the
/// truth about what happens to its git commands.
///
/// `None` when the `rime` binary could not be found or the file could not be
/// written. The session then has no shim, `git` is the real one, and a push to
/// a private remote fails to authenticate the way it does today. Nothing is
/// less safe: the shim holds no credential and enforces nothing.
fn install_git_shim(scratch: &Path) -> Option<PathBuf> {
    install_session_bin(scratch, &rime_program()?, Path::new(TOOL_SHIM_DIR))
}

/// The half of [`install_git_shim`] that names what it installs FROM.
///
/// Split out so a test can drive the real wiring: which shims end up in the
/// one directory that goes first on a session's `PATH` is the property, and a
/// test that called each installer separately would prove each works and not
/// that either is reached.
fn install_session_bin(scratch: &Path, rime: &Path, tools: &Path) -> Option<PathBuf> {
    let bin = scratch.join(SESSION_BIN);
    if let Err(e) = std::fs::create_dir_all(&bin) {
        eprintln!(
            "rime-agentd: could not create {} ({e}), so git is not brokered in this session",
            bin.display()
        );
        return None;
    }
    let shim = bin.join("git");
    let script = format!(
        "#!/bin/sh\n\
         # Written by rime-agentd. `git` for a managed session: push, fetch and\n\
         # ls-remote go through the broker, everything else execs /usr/bin/git.\n\
         exec {} git-shim -- \"$@\"\n",
        rime.display()
    );
    if let Err(e) = std::fs::write(&shim, script) {
        eprintln!(
            "rime-agentd: could not write {} ({e}), so git is not brokered in this session",
            shim.display()
        );
        return None;
    }
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)) {
        eprintln!("rime-agentd: could not make {} executable ({e})", shim.display());
        return None;
    }
    install_tool_shims_from(&bin, tools);
    Some(bin)
}

/// Where the image installs P1-012's `wrangler` and `terraform` shim.
///
/// A directory rather than the script, because what goes in a session's `bin`
/// is one symlink per tool: the shim reads `argv[0]` to decide which tool it is
/// standing in for.
pub const TOOL_SHIM_DIR: &str = "/usr/libexec/rime/tools";

/// §13.4's tool half, put where a session's own `wrangler` will find it.
///
/// ## Why here and not in `/etc/profile.d`
///
/// There is a profile.d drop-in that does the same thing, and it is **not what
/// makes this work for an agent**. `/etc/profile.d/*.sh` is read by a *login*
/// shell. An agent's tool calls are `bash -c '…'` — non-login,
/// non-interactive — and never read it. The drop-in is for a person who opens
/// a terminal inside a managed session; this is for the agent, and this is the
/// one that matters for P1-012's "existing skills can continue invoking normal
/// tools".
///
/// Symlinks into the same `bin` the git shim uses, so there is one directory
/// at the front of the session's `PATH` rather than two, and so an unconfined
/// session gets neither — for the reason the git shim gives: an unconfined
/// session has the user's own tools and the user's own credentials, and no
/// reason to be routed anywhere.
///
/// Best-effort, like the hook settings and for the same reason: a session
/// whose tool shims could not be installed is a session where `wrangler`
/// reaches the real binary with no credential and says so. That is a worse
/// experience, not a hole — nothing here is a boundary, and the note in the
/// shim itself says so.
fn install_tool_shims_from(bin: &Path, source: &Path) {
    for tool in ["wrangler", "terraform"] {
        let from = source.join(tool);
        if !from.exists() {
            // The image did not install it. Not an error: a development build
            // running from a checkout has no /usr/libexec/rime.
            continue;
        }
        let link = bin.join(tool);
        let _ = std::fs::remove_file(&link);
        if let Err(e) = std::os::unix::fs::symlink(&from, &link) {
            eprintln!(
                "rime-agentd: could not link {} ({e}), so {tool} is not brokered in this session",
                link.display()
            );
        }
    }
}

/// The directory inside the session scratch that goes first on its `PATH`.
const SESSION_BIN: &str = "bin";

/// The agent settings file a credential can arrive through.
const SETTINGS_FILE: &str = "settings.json";

/// What the redacted copy is called inside the session scratch. Not
/// `settings.json`: the scratch is bound writable and visible, and two files
/// with the same name and different contents is how somebody debugging this
/// ends up reading the wrong one.
const REDACTED_SETTINGS_FILE: &str = "claude-settings-redacted.json";

/// Write the hook subscriptions for a session, and say where they went.
///
/// `None` when this adapter has no way to be told about a settings file — every
/// agent but Claude today — and also when the write failed or the `rime` binary
/// could not be found. All three are the same thing downstream: no hooks, so
/// the PTY scanner decides state, exactly as it does for an agent nobody has
/// integrated. The failures are logged because a silently unintegrated Claude
/// looks identical to a working one until somebody measures the state.
fn install_hook_settings(
    adapter: &adapter::Adapter,
    scratch: &Path,
    project: Option<&Path>,
    policy: &rime_agent_core::policy::AgentPolicy,
    plugin_allow: &[String],
) -> Option<PathBuf> {
    if !adapter.hooks {
        return None;
    }
    let rime = match rime_program() {
        Some(p) => p,
        None => {
            eprintln!(
                "rime-agentd: no `rime` on PATH, so {} runs without its hook bridge and \
                 reports state from terminal output",
                adapter.id
            );
            return None;
        }
    };
    // The user's own status line, read here rather than inside the settings
    // document, because `hook::settings_json` is pure and this is a filesystem
    // question. See `statusline::overlay` for why the presentation keys have
    // to travel with it and why the command must not.
    let status = rime_agent_core::statusline::user_status_line(&paths::home(), project);

    // Dimension 8. Read here rather than inside the document builder for the
    // same reason the status line is: `hook::settings_json` is pure, and which
    // plugins this machine has enabled is a filesystem question.
    let installed = pluginconf::read(&paths::home());
    let curated = pluginconf::curate(&installed, policy.plugins, plugin_allow);
    if let Some(c) = &curated {
        // Said out loud, because the quiet version of this is a session that
        // silently lost the plugin whose command the user was about to type.
        // `removed_code` is the number that says what the removal bought: a
        // plugin that ships only commands runs nothing on its own, and a
        // report that counted it would overstate the case.
        eprintln!(
            "rime-agentd: {} plugin(s) for this session, {} removed ({} of those ran \
             code of their own — hooks or the scripts beside them — that no MCP \
             confinement would have reached)",
            c.kept.len(),
            c.removed.len(),
            c.removed_code(&installed),
        );
    }

    let path = hook::settings_path(scratch);
    let document = hook::settings_json(&rime, status.as_ref(), curated.as_ref()).to_string();
    match std::fs::write(&path, document) {
        Ok(()) => Some(path),
        Err(e) => {
            eprintln!(
                "rime-agentd: writing {} failed ({e}), so {} runs without its hook bridge",
                path.display(),
                adapter.id
            );
            None
        }
    }
}

/// Write the curated MCP configuration for a session, and say where it went.
///
/// `None` when this adapter cannot be told to use one configuration and ignore
/// the rest, when the write failed, and — the case worth naming — when the
/// session asked for nothing to be reduced and nothing needed confining. All
/// three end the same way downstream: the agent loads the definitions it finds,
/// exactly as it did before this existed.
///
/// That last skip is not laziness. A curated document is `--strict-mcp-config`,
/// and strict means the session's own later edits to `~/.claude.json` stop
/// reaching the agent. Paying that for a session with nothing to confine and
/// nothing to remove would be a behaviour change bought for nothing — so the
/// file is written when the policy reduces something, or when there is
/// third-party executable content to wrap, and not otherwise.
fn install_mcp_config(
    adapter: &adapter::Adapter,
    scratch: &Path,
    workdir: &Path,
    policy: &rime_agent_core::policy::AgentPolicy,
    allow: &[String],
) -> Option<PathBuf> {
    if !adapter.strict_mcp {
        return None;
    }
    let home = paths::home();
    let defs = mcpconf::read(&home, Some(workdir));
    let approval = mcpconf::approvals(&home, Some(workdir));
    // The same resolver the hook bridge uses, and deliberately not a second
    // one: a wrapper that pointed at a different build from the daemon it
    // reports to is the one pairing guaranteed to be wrong.
    let rime = rime_program();
    let curated = mcpconf::curate(&defs, &approval, policy.connectors, allow, rime.as_deref());

    let wraps = curated.confined() > 0;
    if !policy.connectors.reduces() && !wraps {
        return None;
    }

    let path = mcpconf::config_path(scratch);
    let document = curated.document.to_string();
    match std::fs::write(&path, document) {
        Ok(()) => {
            if curated.dropped() > 0 || wraps {
                eprintln!(
                    "rime-agentd: {} connector(s) for this session, {} of them sandboxed \
                     ({} of those by their own definition, so only by this file for the rest), \
                     {} removed",
                    curated.kept(),
                    curated.confined(),
                    curated.confined_everywhere(),
                    curated.dropped()
                );
            }
            Some(path)
        }
        Err(e) => {
            // Loud, because the quiet version of this is a session that looks
            // curated and is not. Every connector the policy meant to remove
            // is reachable after this line.
            eprintln!(
                "rime-agentd: writing {} failed ({e}), so {} starts with the connectors it \
                 finds and NOT the ones this session asked for",
                path.display(),
                adapter.id
            );
            None
        }
    }
}

/// The `rime` binary a hook command will exec, as an absolute path.
///
/// Absolute because the hook runs inside the sandbox, whose `PATH` is the
/// daemon's but whose filesystem is not: a bare `rime` would resolve against
/// directories the home tmpfs has masked.
///
/// The daemon's own sibling first: `rime` and `rime-agentd` are built and
/// shipped together, and a hook command that ran a different build from the
/// daemon it reports to is the one pairing guaranteed to be wrong. Skipped when
/// a session could not exec it anyway — the masked home and the private `/tmp`
/// are the two places a confined process cannot see, which is the test
/// [`bridge_program`] already applies. Then `/usr/bin/rime`, where the image
/// puts it, and finally the daemon's `PATH`.
fn rime_program() -> Option<PathBuf> {
    let sibling = std::env::current_exe().ok().and_then(|exe| {
        let p = exe.parent()?.join("rime");
        let reachable = !p.starts_with("/tmp") && !p.starts_with(paths::home());
        (p.is_file() && reachable).then_some(p)
    });
    if sibling.is_some() {
        return sibling;
    }
    let installed = PathBuf::from("/usr/bin/rime");
    if installed.is_file() {
        return Some(installed);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join("rime"))
        .find(|p| p.is_file())
}

/// The `PATH` a session inherits.
///
/// Taken from the daemon's environment. That is NOT the login environment,
/// which this comment used to claim: the daemon starts at boot with whatever
/// the user manager had then — `/usr/local/sbin:/usr/local/bin:/usr/bin` on
/// the L16, read from `/proc/<pid>/environ` — and the login session adds
/// `~/.local/bin` to the manager only later, so a daemon started first never
/// sees it. `npm i -g @openai/codex` with the npm prefix at `~/.local` puts
/// `codex` exactly there, and `a -a codex` said codex was not installed. The
/// unit (`files/system/units/rime-agentd.service`) now sets the user
/// directories itself, and this hands the same list to every session, so a
/// tool an agent runs from `~/.local/bin` resolves too. The sandbox decides
/// separately whether those directories are actually visible.
fn inherited_path() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string())
}

/// The `WAYLAND_DISPLAY` a session should start with, if it should have one.
///
/// Pasting an image into an agent is the agent CLI reading the clipboard
/// itself — `wl-paste` for Claude Code, a Wayland clipboard crate for Codex —
/// and both find the compositor through `WAYLAND_DISPLAY`. This daemon has
/// none (`clipboard.rs` says why: it starts before any compositor exists and
/// nothing imports the variable afterwards), and an unconfined session's
/// environment is the daemon's, so Ctrl+V of a screenshot silently did
/// nothing. `probe` finds the live socket the way the clipboard verb does.
///
/// Unconfined sessions only. A confined one masks `/run`, so the socket is
/// not there for it to reach, and a disposable session's child is the
/// container engine, not the agent. No compositor (a TTY, an ssh login) is no
/// variable, the same as before.
fn session_display(
    confined: bool,
    disposable: bool,
    probe: impl FnOnce() -> std::result::Result<String, String>,
) -> Option<(String, String)> {
    if confined || disposable {
        return None;
    }
    probe().ok().map(|display| ("WAYLAND_DISPLAY".to_string(), display))
}

/// This runtime's own binary, which is also the egress bridge.
///
/// Checked for existence rather than trusted, because `/proc/self/exe` answers
/// with a path that ends in " (deleted)" once the file behind it is gone, and
/// because a session's view of the filesystem is not the daemon's: `$HOME` and
/// `/tmp` are masked inside the sandbox, so a development build living in
/// either is a path the session cannot exec. Both cases would otherwise
/// surface as a session that starts and dies with status 127.
fn bridge_program() -> Result<PathBuf> {
    let program = std::env::current_exe()
        .context("finding this runtime's own binary, which is the egress bridge")?;
    if !program.is_file() {
        bail!(
            "the egress bridge for an allowlisted session is this runtime's own binary, and \
             {} is not there any more; restart the runtime, or use `--network offline`",
            program.display()
        );
    }
    // `/tmp` only. `/var/tmp` is covered by the read-only root like the rest of
    // the filesystem, and a build there is reachable — which is what makes a
    // live test of this mode possible at all.
    if program.starts_with("/tmp") {
        bail!(
            "an allowlisted session cannot reach {}, because a confined session's /tmp is a \
             fresh tmpfs; install the runtime, or use `--network offline`",
            program.display()
        );
    }
    if program.starts_with(paths::home()) {
        bail!(
            "an allowlisted session cannot reach {}, because a confined session's home is \
             masked; install the runtime, or use `--network offline`",
            program.display()
        );
    }
    Ok(program)
}

/// Whether this runtime is running as root.
///
/// Only consulted for the agent's own permission mode, which some upstream
/// CLIs refuse under root. Nothing in Rime's own policy branches on it: root
/// is a dimension, not a euid.
fn is_root() -> bool {
    // Safe: geteuid takes no arguments and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// A sandbox refusal, carried so `dispatch` can map it to the right error kind.
#[derive(Debug)]
pub struct SandboxRefused(pub SandboxError);

impl std::fmt::Display for SandboxRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SandboxRefused {}

/// A permission-dimension refusal, carried for the same reason.
///
/// Kept distinct from [`SandboxRefused`] because the two need different
/// remedies: a sandbox refusal is answered with `--sandbox unrestricted`, and
/// telling somebody denied a system grant to loosen their sandbox would be
/// advice that both fails and makes them less safe.
#[derive(Debug)]
pub struct PolicyRefused(pub PolicyError);

impl std::fmt::Display for PolicyRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PolicyRefused {}

/// An origin that could not be established, or a declaration that was refused.
///
/// Its own type, because the remedy is neither of the two above: nothing about
/// the sandbox or the six dimensions will help, and the message already says
/// which `/proc` read failed or which restriction the declaration tried to
/// drop.
#[derive(Debug)]
pub struct OriginRefused(pub String);

impl std::fmt::Display for OriginRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for OriginRefused {}

/// §36's `[identity.*]` refused this session. P2-013.
///
/// Its own type for the same reason [`OriginRefused`] is: the remedy is
/// neither the sandbox nor a policy dimension. It is a line in the project's
/// own `rime.toml`, and the message says which one.
#[derive(Debug)]
pub struct IdentityRefused(pub String);

impl std::fmt::Display for IdentityRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for IdentityRefused {}

/// A system-access grant that was refused, or a TTL that was not issuable.
///
/// Its own type for the same reason as the three above: the remedies are
/// different and specific. A grant refused because the connection came from
/// inside a session is answered by asking from a terminal; one refused because
/// the origin was remote is answered by approving locally; one refused because
/// polkit said no is answered by getting the password right. None of them is
/// answered by changing the sandbox, which is what a shared error type would
/// eventually suggest.
#[derive(Debug)]
pub struct GrantRefused(pub String);

impl std::fmt::Display for GrantRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for GrantRefused {}

/// A dimension refusal that is a sentence rather than a [`PolicyError`].
///
/// The TTL bounds live in `grant.rs`, which knows nothing about `PolicyError`
/// and should not: a TTL is not one of the six dimensions, it is a parameter
/// of a grant.
#[derive(Debug)]
pub struct TtlRefused(pub String);

impl std::fmt::Display for TtlRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TtlRefused {}

/// A `--capabilities` list this build will not issue a grant for (P0-007).
///
/// Its own type rather than folded into [`TtlRefused`], following the same
/// rule the four above follow: the remedies are different and specific. A
/// refused TTL is answered by asking for a shorter window; a refused
/// capability list is answered by spelling the verb correctly, or by not
/// narrowing a break-glass grant that has no verbs to narrow.
#[derive(Debug)]
pub struct CapabilitiesRefused(pub String);

impl std::fmt::Display for CapabilitiesRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CapabilitiesRefused {}

/// The allowlist ONE session runs under, out of the one the runtime carries
/// (P2-012, [`rime_agent_core::protocol::RunRequest::allow`]).
///
/// Split out of [`start`] so the rule can be asserted without a daemon, a
/// sandbox, a PTY or a network — the same reason the CLI's version table is
/// split out of the call that uses it. A rule that can only be exercised by
/// starting a real confined session is a rule nobody exercises, and this one
/// decides where a session can connect.
///
/// Three answers, and the two refusals are refusals rather than repairs:
///
/// * no narrowing asked for — the runtime's list, unchanged, which is what
///   every allowlisted session got before this existed;
/// * a narrowing on a session with no allowlist to narrow — REFUSED. An
///   `open` session reaches everything and an `offline` one reaches nothing,
///   and in both the list means nothing at all. Silently accepting it would
///   print a boundary in `rime agent status` that no proxy enforces, which is
///   `ttl_ms` on an ordinary session one dimension over;
/// * a narrowing — validated by [`Allowlist::narrow`], which refuses any line
///   the runtime does not already cover.
fn session_allowlist(
    runtime: &Allowlist,
    requested: Option<&[String]>,
    network: NetworkPolicy,
) -> Result<Allowlist, AllowlistRefused> {
    let Some(lines) = requested else {
        return Ok(runtime.clone());
    };
    if network != NetworkPolicy::Allowlist {
        return Err(AllowlistRefused(format!(
            "`allow` names destinations for a session whose network is `{}`, which has no \
             allowlist to narrow. re-run with `--network allowlist`, or drop `--allow`",
            network.as_str()
        )));
    }
    runtime
        .narrow(lines)
        .map_err(|e| AllowlistRefused(e.to_string()))
}

/// P2-012 route B: the refusals that are facts about the request alone.
///
/// Separated from [`present_pin`] for [`crate::browser_ca::check`]'s reason:
/// these three cost nothing and a session refused here has not had a scratch
/// directory, a worktree or a certificate made for it.
fn present_check(
    present: Option<&str>,
    trust_ca: Option<&str>,
    confined: bool,
    network: NetworkPolicy,
) -> Result<()> {
    let Some(service) = present else {
        return Ok(());
    };
    if !confined {
        // `--ttl` on a session with no grant, one field over. An unconfined
        // session has no mount namespace, so the minted CA could not be
        // installed in its browser — the capsule would refuse the very
        // connection this was asked for, and blame its own timeout.
        bail!(
            "present authenticates one destination by terminating its TLS with a certificate \
             installed inside the session's mount namespace, and an unconfined session does \
             not have one. Ask for a confined sandbox, or drop the present"
        );
    }
    if network != NetworkPolicy::Allowlist {
        // The interception lives in the egress proxy, and the egress proxy is
        // the allowlist's only route out. Without it there is no proxy, the
        // session reaches the host's network directly, and the field would be
        // accepted and mean nothing.
        bail!(
            "present is enforced by the egress proxy, which only exists for `--network \
             allowlist`. This session asked for {network}, so the capsule would reach the \
             site itself and carry no credential"
        );
    }
    if trust_ca.is_some() {
        // Refused rather than combined, and the reason is not that it is hard.
        // A session with `present` is pinned to ONE destination and this
        // daemon terminates that one, so there is no connection left for a
        // caller-supplied root to be about; a caller who passed both believes
        // one of the two is doing something it is not.
        bail!(
            "trust_ca and present cannot both be asked for: present pins this session to the \
             one destination '{service}' is stored for and terminates it here, so a \
             certificate authority for the capsule's own connection to that site would never \
             be used. Drop one"
        );
    }
    Ok(())
}

/// The destination a credential is pinned to, and the proof that it is the
/// only one this session can reach.
///
/// The pin comes from the SECRET SERVICE, not from the caller. That is the
/// whole of why `present` is one field: a wire field naming the destination
/// would be a second thing that can disagree with the credential, and the
/// disagreement would be settled in favour of whichever one the caller wrote.
///
/// The allowlist comparison is against a list PARSED from the pin rather than
/// against a string, because `Rule::as_line` drops a default port — so
/// `intranet.example:443` and `intranet.example` are the same rule and would
/// not be the same string.
fn present_pin(service: &str, allowlist: &Allowlist) -> Result<Destination> {
    use rime_secret_core::client::Client as SecretClient;
    use rime_secret_core::protocol::{Request as SecretRequest, Response as SecretResponse};

    let answer = SecretClient::connect()
        .and_then(|mut c| c.call(&SecretRequest::List))
        .with_context(|| {
            format!(
                "present names the stored credential '{service}', and the secret service \
                 could not be asked what it is pinned to"
            )
        })?;
    let SecretResponse::Services { services } = answer else {
        bail!("the secret service answered a list with something else");
    };
    let Some(info) = services.into_iter().find(|s| s.service == service) else {
        bail!(
            "no credential named '{service}' is stored, so there is no destination for this \
             capsule to be authenticated to. `rime secret list` shows what is stored"
        );
    };
    let Some(port) = info.port.or(match info.scheme.as_str() {
        "https" => Some(443),
        "http" => Some(80),
        _ => None,
    }) else {
        bail!(
            "'{service}' is stored for scheme '{}', which has no port a capsule could be \
             pinned to",
            info.scheme
        );
    };
    let pin = Destination::new(&info.host, port).with_context(|| {
        format!("'{service}' is pinned to a host this cannot be a destination for")
    })?;

    present_allowlist_is_only(&pin, allowlist)?;
    Ok(pin)
}

/// The session must be able to reach the pin and nothing else.
///
/// Not "the allowlist covers it". A wildcard rule covering the pin would leave
/// the capsule able to reach hosts the credential was never for, through
/// tunnels this daemon does not read — so a caller who asked for one
/// authenticated destination would have got several unauthenticated ones
/// beside it, and the screenshot would not say which was which.
///
/// Compared against a list PARSED from the pin rather than against a string,
/// because `Rule::as_line` drops a default port: `intranet.example:443` and
/// `intranet.example` are the same rule and are not the same string.
///
/// Split from [`present_pin`] so it can be exercised without a secret service,
/// which is the half of that function that needs one.
fn present_allowlist_is_only(pin: &Destination, allowlist: &Allowlist) -> Result<()> {
    let want = Allowlist::parse(&[pin.to_string()])
        .map_err(|e| anyhow!("{pin} is not a destination this can be an allowlist for: {e}"))?;
    if allowlist.lines() != want.lines() {
        bail!(
            "present pins this session to {pin}, and its allowlist is [{}]. A session that \
             names a credential may reach exactly the destination that credential is for: \
             pass `--allow {pin}` and nothing else",
            allowlist.lines().join(", ")
        );
    }
    Ok(())
}

/// The capability record every intercepted connection is opened with.
///
/// Built ONCE, here, out of what this daemon knows about the session it is
/// starting — never out of anything the capsule can reach. The capsule has no
/// way to ask for an interception at all: it opens a `CONNECT` like any other,
/// and whether that `CONNECT` is terminated was decided before the browser
/// existed.
///
/// The project is `detect`-or-cwd, the same rule the capsule binding at the
/// top of this function uses and the same rule `rime secret` grants are keyed
/// on. For a browser capsule that is the throwaway capsule directory, which is
/// why `browser.present` is an operation an owner grants with `--everywhere`:
/// a grant recorded against one capsule's directory would name a path that is
/// gone by the time a second capsule runs.
fn present_record(
    service: &str,
    project: &Path,
    session: u32,
    origin: RequestOrigin,
    source: OriginSource,
) -> rime_secret_core::capability::CapabilityRecord {
    let mut record = rime_secret_core::capability::CapabilityRecord::new(
        service,
        rime_secret_core::operation::BROWSER_PRESENT,
        "",
    );
    record.project = Some(project.to_string_lossy().into_owned());
    // Attribution and not authentication, exactly as `broker::use_capability`
    // says: `rime-secretd` cannot re-derive either of these, so it records the
    // claim and authorises on what it can establish for itself.
    record.agent_session = Some(session);
    record.request_origin = origin.as_str().to_string();
    record.origin_source = source.as_str().to_string();
    record
}

/// A session allowlist that could not be narrowed out of the runtime's
/// (P2-012, [`rime_agent_core::protocol::RunRequest::allow`]).
///
/// Its own type for the reason [`CapabilitiesRefused`] is its own type: the
/// remedy is specific and nothing else's remedy fits it. A refused narrowing
/// is answered by `rime agent allow <destination>` — adding the destination to
/// the runtime's list once, deliberately — or by not naming it. It is never
/// answered by authorising anything, because the session already has every
/// permission it needs; it asked for FEWER destinations than the machine
/// permits and named one the machine does not permit at all.
#[derive(Debug)]
pub struct AllowlistRefused(pub String);

impl std::fmt::Display for AllowlistRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for AllowlistRefused {}

/// A resource budget that cannot be delivered (§P2-011).
///
/// Its own type rather than a bare `anyhow!` because the kind is the point: a
/// budget refusal is never fixed by authorising anything, and it is never the
/// caller's request that is wrong — it is the machine or the configuration —
/// so `BadRequest` would send the user to look in the wrong place. The rule
/// behind every one of these is the same: a budget that is silently not
/// applied is worse than a session that did not start.
#[derive(Debug)]
pub struct BudgetRefused(pub String);

impl std::fmt::Display for BudgetRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for BudgetRefused {}

/// Map a `start` failure to a response, keeping the distinctions the client
/// needs in order to explain what to do next.
pub fn run_error(e: anyhow::Error) -> Response {
    // The whole chain, not just the innermost error: the sandbox refusal
    // carries the remedy ("re-run with --sandbox unrestricted") and the outer
    // context says which step was refused.
    if e.downcast_ref::<SandboxRefused>().is_some() {
        return Response::error(ErrorKind::SandboxUnavailable, format!("{e:#}"));
    }
    if e.downcast_ref::<PolicyRefused>().is_some() {
        return Response::error(ErrorKind::PolicyRefused, format!("{e:#}"));
    }
    if e.downcast_ref::<OriginRefused>().is_some() {
        return Response::error(ErrorKind::PermissionDenied, format!("{e:#}"));
    }
    // A refused grant is a permission answer, so it gets the kind a client
    // branches on for one. A refused TTL is the user asking for something
    // out of bounds, which is a bad request.
    if e.downcast_ref::<GrantRefused>().is_some() {
        return Response::error(ErrorKind::PermissionDenied, format!("{e:#}"));
    }
    if e.downcast_ref::<TtlRefused>().is_some() {
        return Response::error(ErrorKind::PolicyRefused, format!("{e:#}"));
    }
    if e.downcast_ref::<CapabilitiesRefused>().is_some() {
        return Response::error(ErrorKind::PolicyRefused, format!("{e:#}"));
    }
    if e.downcast_ref::<BudgetRefused>().is_some() {
        return Response::error(ErrorKind::PolicyRefused, format!("{e:#}"));
    }
    // A refused narrowing is a policy answer and not a `BadRequest`: the
    // request was well formed and the destination policy is what refused it.
    // Falling through to the default arm would tell a capsule's caller their
    // request was malformed, and send them to look at their own command line
    // instead of at `rime agent allow`.
    if e.downcast_ref::<AllowlistRefused>().is_some() {
        return Response::error(ErrorKind::PolicyRefused, format!("{e:#}"));
    }
    // A project binding is policy, not a privilege decision: nothing about
    // this session's grant or origin would change the answer, and a client
    // that offered to re-ask for root would be offering the wrong remedy.
    if e.downcast_ref::<IdentityRefused>().is_some() {
        return Response::error(ErrorKind::PolicyRefused, format!("{e:#}"));
    }
    Response::error(ErrorKind::BadRequest, format!("{e:#}"))
}

/// Read a session's terminal until the process ends.
fn spawn_reader(daemon: Arc<Daemon>, handle: Handle, id: u32) {
    let name = format!("rime-agentd-s{id}");
    let worker = Arc::clone(&handle);
    let spawned = std::thread::Builder::new()
        .name(name)
        .spawn(move || reader_loop(&daemon, &worker));
    if spawned.is_err() {
        // Without a reader the session would produce no output and never be
        // reaped, which is worse than not having started it.
        let mut s = handle.lock().expect("session lock");
        registry::terminate(&mut s);
        s.set_exited(Some(-1), None);
        registry::write_record(&s.info);
    }
}

fn reader_loop(daemon: &Arc<Daemon>, handle: &Handle) {
    let (master, pid, id) = {
        let s = handle.lock().expect("session lock");
        (s.master, s.pid, s.info.id)
    };
    let mut buf = vec![0u8; 64 * 1024];

    loop {
        let readable = pty::wait_readable(master, POLL_INTERVAL_MS);

        let mut ended = false;
        if readable {
            match pty::read_nonblocking(master, &mut buf) {
                Ok(Some(0)) => {}
                Ok(Some(n)) => absorb(handle, &buf[..n]),
                // EOF or EIO: the child closed the terminal.
                Ok(None) => ended = true,
                Err(_) => ended = true,
            }
        }

        match pty::try_wait(pid) {
            pty::Wait::Running => {
                if ended {
                    // The terminal closed but the process is still around —
                    // it detached or handed the PTY to a child that exited.
                    // Keep waiting rather than reporting a session that is
                    // still burning CPU as finished.
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    continue;
                }
                update_idle_state(handle);
            }
            pty::Wait::Exited(code) => {
                drain(handle, master, &mut buf);
                finish(daemon, handle, Some(code), None);
                break;
            }
            pty::Wait::Signalled(sig) => {
                drain(handle, master, &mut buf);
                finish(daemon, handle, None, Some(sig));
                break;
            }
            pty::Wait::Gone => {
                drain(handle, master, &mut buf);
                finish(daemon, handle, Some(-1), None);
                break;
            }
        }

        if handle.lock().expect("session lock").closing {
            break;
        }
    }

    let _ = id;
}

/// Push output through the scanner and into the session.
fn absorb(handle: &Handle, data: &[u8]) {
    let mut s = handle.lock().expect("session lock");
    let signals = s.scanner.feed(data);
    s.absorb(data);
    let in_flight = s.tool_in_flight();
    let next = logic::next_state(s.info.state, &signals, true, 0, in_flight);
    let detail = signals
        .iter()
        .rev()
        .find_map(|sig| sig.detail().map(|d| d.to_string()));
    s.set_state(next, detail);
    if retitle(&mut s.info, &signals) {
        // Only on a CHANGE. Claude retitles itself on every spinner frame
        // while it works; with the glyphs stripped those are one title, so
        // this writes when the summary changes and not ten times a second.
        registry::write_record(&s.info);
    }
}

/// Apply the last title in a read to a session, and say whether it changed.
///
/// The last one, because a read can carry several and the terminal would show
/// the last. A title of nothing clears the field: the agent took its title
/// away, and a stale one would outlive it.
fn retitle(info: &mut SessionInfo, signals: &[logic::Signal]) -> bool {
    let Some(title) = signals.iter().rev().find_map(|sig| match sig {
        logic::Signal::Title(t) => Some(t),
        _ => None,
    }) else {
        return false;
    };
    if info.title == *title {
        return false;
    }
    info.title = title.clone();
    true
}

/// Re-evaluate state for a session that produced nothing this tick.
fn update_idle_state(handle: &Handle) {
    let mut s = handle.lock().expect("session lock");
    if !s.info.is_live() {
        return;
    }
    let idle = s.idle_secs();
    let in_flight = s.tool_in_flight();
    let next = logic::next_state(s.info.state, &[], false, idle, in_flight);
    if next != s.info.state {
        s.set_state(next, None);
        // Recording only on a change keeps an idle session from rewriting its
        // record once a second for hours.
        registry::write_record(&s.info);
    }
}

/// Read whatever the terminal still holds after the process exited.
fn drain(handle: &Handle, master: libc::c_int, buf: &mut [u8]) {
    for _ in 0..64 {
        match pty::read_nonblocking(master, buf) {
            Ok(Some(0)) | Ok(None) | Err(_) => break,
            Ok(Some(n)) => absorb(handle, &buf[..n]),
        }
    }
}

/// Record the exit and release the terminal.
fn finish(daemon: &Arc<Daemon>, handle: &Handle, code: Option<i32>, signal: Option<i32>) {
    let (id, master) = {
        let mut s = handle.lock().expect("session lock");
        s.set_exited(code, signal);
        registry::write_record(&s.info);
        (s.info.id, s.master)
    };

    pty::close(master);
    {
        let mut s = handle.lock().expect("session lock");
        s.master = -1;
    }

    // The scratch directory is the session's, and nothing outside it should be
    // holding a path into it once the session is gone.
    let _ = std::fs::remove_dir_all(paths::scratch_dir(id));

    // Keep the record in the registry so `rime agent list` still shows the
    // outcome; `rime agent prune` is what clears it.
    let _ = daemon;
}

/// Turn a control connection into a session's terminal.
///
/// The response line goes out first, then the connection carries only PTY
/// bytes in both directions.
#[allow(clippy::too_many_arguments)]
pub fn handle_attach(
    daemon: &Arc<Daemon>,
    caller: &Caller,
    mut writer: UnixStream,
    reader: BufReader<UnixStream>,
    id: u32,
    cols: u16,
    rows: u16,
    replay: usize,
) -> Result<()> {
    // Whose size this attach is (§1.7): a phone's, or the desktop's. Worked
    // out before any session lock is taken, because resolving an origin walks
    // the registry and takes each session's lock in turn, and no code path
    // holds a session lock while it takes another.
    let viewer = crate::privilege::viewer(&crate::privilege::origin(daemon, caller));

    let Some(handle) = daemon.registry.lock().expect("registry lock").get(id) else {
        let resp = Response::error(ErrorKind::NoSuchSession, format!("no session {id}"));
        return write_response(&mut writer, &resp);
    };

    {
        let s = handle.lock().expect("session lock");
        if !s.info.is_live() {
            let resp = Response::error(
                ErrorKind::SessionExited,
                format!(
                    "session {id} has already {}; use `rime agent logs {id}` to read its output",
                    s.info.exit_summary().unwrap_or_else(|| "exited".into())
                ),
            );
            return write_response(&mut writer, &resp);
        }
    }

    write_response(&mut writer, &Response::Attached { id })?;

    let mirror = writer.try_clone().context("cloning for output mirroring")?;
    {
        let mut s = handle.lock().expect("session lock");
        // Adopt the attaching terminal's size, so the agent repaints
        // correctly — and remember it for this kind of viewer, so a phone
        // that leaves can give the desktop's back.
        //
        // One lock with the registration below rather than two. Between two,
        // another phone detaching would see no phone attached and give the
        // desktop its size back underneath the phone that is arriving.
        let size = WinSize { cols, rows }.or_fallback();
        let _ = s.attach_size(viewer, size);
        // Register the output direction before replaying, so nothing produced
        // between the two is lost.
        if let Err(e) = s.attach_as(mirror, replay, viewer) {
            // A phone that failed to attach after its size was applied is a
            // phone that is not looking; the desktop gets its size back.
            s.settle_size();
            return Err(e);
        }
    }

    // This thread becomes the input pump. It ends when the client shuts down
    // its write half (a detach) or disconnects.
    let mut source = reader.into_inner();
    let mut buf = [0u8; 8192];
    loop {
        let n = match source.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let live_master = {
            let mut s = handle.lock().expect("session lock");
            if !s.info.is_live() {
                break;
            }
            // The person typing is the person looking (§1.7, tmux's
            // `window-size latest`): keystrokes from this viewer give the
            // terminal this viewer's size back if another kind took it.
            // Two compares under a lock this loop already takes; a resize only
            // when the sizes differ.
            s.typed(viewer, &buf[..n]);
            s.master
        };
        if live_master < 0 || pty::write_all(live_master, &buf[..n]).is_err() {
            break;
        }
    }

    // Detaching removes this client and nothing else: the session keeps
    // running, which is the whole point of the runtime owning the PTY.
    detach(&handle, &writer);
    Ok(())
}

/// What came of writing into a session's terminal.
///
/// Three outcomes and not a `Result`, because two of the three are ordinary
/// answers a client acts on differently: an exited session means "pick another
/// target", an I/O failure means "the terminal is broken".
pub enum Input {
    Written,
    Exited,
    Failed(String),
}

/// Write bytes into a live session's terminal.
///
/// This is the input pump of [`handle_attach`] with the loop taken off: the
/// daemon owns the PTY master, so a client with nothing to display does not
/// need to become the terminal to be heard. `Request::Input` is the caller.
///
/// The master descriptor is copied out and the lock RELEASED before the write,
/// which is the whole reason this is a function rather than four lines in the
/// dispatch arm. `pty::write_all` blocks when the agent is not draining its
/// input: it waits for writability rather than spinning, so a TUI that has
/// paused its reader can hold the write open indefinitely. Holding the session
/// lock across that would freeze every other verb for the session, including
/// the `Signal` a user reaches for precisely when an agent has stopped
/// reading — the deadlock would be worst at the only moment it mattered. The
/// pump above takes the lock the same way for the same reason; `Resize` is the
/// one that holds it across the syscall, and `pty::resize` cannot block.
pub fn write_input(handle: &Handle, data: &[u8]) -> Input {
    let master = {
        let s = handle.lock().expect("session lock");
        if !s.info.is_live() {
            return Input::Exited;
        }
        s.master
    };
    // A live session with a closed master is a race, not a state: the reaper
    // sets the fd to -1 as the child goes away. Writing to -1 would be an
    // EBADF reported as a broken terminal, when the truth is the same as the
    // check above.
    if master < 0 {
        return Input::Exited;
    }
    match pty::write_all(master, data) {
        Ok(()) => Input::Written,
        Err(e) => Input::Failed(e.to_string()),
    }
}

/// Write text into a session and then press Enter, as two writes (§1.2).
///
/// `data`, then [`logic::SUBMIT_GAP`] with NO lock held — [`write_input`]
/// releases the session lock before it writes, and the sleep is between two
/// calls to it — then `\r` on its own. The gap is the whole point: an agent
/// TUI that reads a burst ending in CR as one chunk treats it as a paste, and
/// a pasted CR is a newline in the prompt rather than Enter. A lock held
/// across the sleep would freeze every other verb for the session for as long
/// as it lasted, which is `write_input`'s own reason for existing.
///
/// Nothing is stripped from `data`. An empty `data` is Enter alone, with no
/// gap in front of it — there is no burst to separate it from.
pub fn submit_input(handle: &Handle, data: &[u8]) -> Input {
    if !data.is_empty() {
        match write_input(handle, data) {
            Input::Written => {}
            other => return other,
        }
        std::thread::sleep(logic::SUBMIT_GAP);
    }
    write_input(handle, b"\r")
}

/// Rename a live session (§1.5), and answer with what was recorded.
///
/// The caller has already been allowed ([`crate::privilege::refuse_rename`])
/// and the name already checked; this is the part that needs the session.
/// Live sessions only: an exited one is `session_exited`, the answer every
/// other verb gives a session the daemon still holds but that has finished.
pub fn rename(handle: &Handle, name: Option<String>) -> Response {
    let mut s = handle.lock().expect("session lock");
    if !s.info.is_live() {
        return Response::error(
            ErrorKind::SessionExited,
            format!("session {} has already exited", s.info.id),
        );
    }
    s.info.name = name;
    registry::write_record(&s.info);
    Response::Session(Box::new(s.info.clone()))
}

/// The tail of a session's output, base64 (§1.6).
///
/// From the in-memory scrollback, which is exactly what a reattaching terminal
/// is repainted with, and never the transcript on disk. Nothing about the
/// session moves: no resize, no attach, `attached` untouched.
///
/// An exited session the daemon still holds is answered too — its scrollback
/// is still there, and "what did it end on" is a question a phone asks of a
/// session that just finished. `Logs` answers it the same way.
pub fn peek(handle: &Handle, bytes: Option<usize>) -> Response {
    let want = bytes.unwrap_or(logic::PEEK_MAX).min(logic::PEEK_MAX);
    let s = handle.lock().expect("session lock");
    let tail = s.scrollback.tail(want);
    Response::Peek {
        id: s.info.id,
        data: rime_agent_core::webauthn::b64_encode(&tail),
        cols: s.info.cols,
        rows: s.info.rows,
        state: s.info.state,
    }
}

/// Remove one attached client from a session.
fn detach(handle: &Handle, stream: &UnixStream) {
    use std::os::unix::io::AsRawFd;
    let target = stream.as_raw_fd();
    let mut s = handle.lock().expect("session lock");
    // Compare by the peer's identity rather than by index: another client may
    // have detached while this one was reading.
    s.attachers.retain(|a| !same_peer(a.stream.as_raw_fd(), target));
    s.info.attached = s.attachers.len() as u32;
    // If that was the last phone, the desktop gets its size back (§1.7).
    s.settle_size();
}

/// Whether two descriptors refer to the same socket.
///
/// `try_clone` produces a different descriptor number for the same open file
/// description, so the numbers cannot be compared directly; `st_ino` on a
/// socket identifies the socket itself.
fn same_peer(a: libc::c_int, b: libc::c_int) -> bool {
    fn inode(fd: libc::c_int) -> Option<u64> {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // Safe: fstat writes one struct we own.
        if unsafe { libc::fstat(fd, &mut st) } != 0 {
            return None;
        }
        Some(st.st_ino)
    }
    match (inode(a), inode(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

pub(crate) fn write_response(writer: &mut UnixStream, response: &Response) -> Result<()> {
    let mut line = serde_json::to_string(response)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush().ok();
    Ok(())
}

/// What to tell someone whose agent CLI is missing. Claude Code is the default
/// agent and left the image on 2026-10-04: it installs per user, and the one
/// command that does it is worth naming rather than "install it".
fn install_hint(program: &str) -> &'static str {
    match program {
        "claude" => {
            "install it with `rime install claude-code`, \
             or run a different agent with `rime agent run --agent <name>`"
        }
        _ => "install it, or run a different agent with `rime agent run --agent <name>`",
    }
}

#[cfg(test)]
mod tests {

    /// The default agent left the image: a missing `claude` names the command
    /// that installs it, and any other missing program keeps the generic text.
    #[test]
    fn a_missing_claude_names_rime_install_claude_code() {
        use super::install_hint;
        assert!(install_hint("claude").contains("rime install claude-code"));
        assert!(!install_hint("codex").contains("claude-code"));
        assert!(install_hint("codex").starts_with("install it"));
    }

    /// Image paste: an unconfined session is told where the compositor is,
    /// and nothing else is. The first assertion is the regression — before
    /// it, every session ran with the daemon's environment and no display.
    #[test]
    fn only_an_unconfined_session_is_handed_the_display() {
        use super::session_display;
        let live = || Ok::<_, String>("wayland-1".to_string());

        assert_eq!(
            session_display(false, false, live),
            Some(("WAYLAND_DISPLAY".to_string(), "wayland-1".to_string()))
        );
        // Confined and disposable are refused without asking the probe at all.
        let untouched = || -> Result<String, String> { panic!("probed for a session that gets no display") };
        assert_eq!(session_display(true, false, untouched), None);
        assert_eq!(session_display(false, true, untouched), None);
        // No compositor: the session still starts, with no variable.
        assert_eq!(session_display(false, false, || Err("no compositor".to_string())), None);
    }

    /// P2-012 route B: the three refusals that are facts about the request.
    ///
    /// Each one is a setting that would otherwise be accepted and mean
    /// nothing, which is the failure mode `--ttl` on a session with no grant
    /// established the shape of.
    #[test]
    fn present_is_refused_where_it_would_be_accepted_and_do_nothing() {
        use rime_agent_core::policy::SandboxPolicy;

        // Nothing asked for is always fine, including on a session that could
        // not have carried it.
        assert!(present_check(None, None, false, NetworkPolicy::Open).is_ok());

        // The shape that works.
        assert!(present_check(Some("intranet"), None, true, NetworkPolicy::Allowlist).is_ok());

        // Unconfined: no mount namespace, so the minted CA could not be
        // installed and the capsule would refuse the very connection this was
        // asked for — and blame its own timeout.
        let e = present_check(Some("intranet"), None, false, NetworkPolicy::Allowlist)
            .expect_err("unconfined");
        assert!(e.to_string().contains("mount namespace"), "{e}");

        // No allowlist: no egress proxy, so nothing would intercept anything
        // and the capsule would reach the site itself, as nobody.
        for network in [NetworkPolicy::Open, NetworkPolicy::Offline, NetworkPolicy::Brokered] {
            let e = present_check(Some("intranet"), None, true, network).expect_err("network");
            assert!(
                e.to_string().contains("egress proxy"),
                "{network}: {e}"
            );
        }

        // Both: the capsule has one destination and this daemon terminates it,
        // so a caller-supplied root would be for a connection that no longer
        // exists. A caller who passed both believes one of them is doing
        // something it is not.
        let e = present_check(
            Some("intranet"),
            Some("/etc/pki/root.pem"),
            true,
            NetworkPolicy::Allowlist,
        )
        .expect_err("the pair");
        assert!(e.to_string().contains("cannot both be asked for"), "{e}");

        let _ = SandboxPolicy::default();
    }

    /// The allowlist a session with `present` may have is the pin, exactly.
    #[test]
    fn a_session_that_names_a_credential_may_reach_that_destination_and_no_other() {
        let pin = Destination::parse("intranet.example:443").expect("pin");

        // The same destination, spelled with and without the default port,
        // because `Rule::as_line` drops 443 and a string comparison would
        // refuse the spelling `rime browser` actually sends.
        for line in ["intranet.example", "intranet.example:443"] {
            let allow = Allowlist::parse(&[line]).expect("parse");
            assert!(
                present_allowlist_is_only(&pin, &allow).is_ok(),
                "--allow {line} is the pin and was refused"
            );
        }

        // A wildcard that COVERS the pin is not the pin: everything else it
        // covers would be a tunnel this daemon does not read, beside one
        // destination it does.
        let wildcard = Allowlist::parse(&["*.intranet.example"]).expect("parse");
        let pin = Destination::parse("eu.intranet.example:443").expect("pin");
        let e = present_allowlist_is_only(&pin, &wildcard).expect_err("wildcard");
        assert!(e.to_string().contains("exactly the destination"), "{e}");

        let pin = Destination::parse("intranet.example:443").expect("pin");
        // One extra destination is one unauthenticated destination.
        let two = Allowlist::parse(&["intranet.example", "elsewhere.example"]).expect("parse");
        assert!(present_allowlist_is_only(&pin, &two).is_err());

        // Another port on the same host is another endpoint.
        let other_port = Allowlist::parse(&["intranet.example:8443"]).expect("parse");
        assert!(present_allowlist_is_only(&pin, &other_port).is_err());

        // And a list that does not contain it at all.
        let elsewhere = Allowlist::parse(&["elsewhere.example"]).expect("parse");
        assert!(present_allowlist_is_only(&pin, &elsewhere).is_err());
    }

    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::io::RawFd;
    use std::path::PathBuf;

    #[test]
    fn a_sandbox_refusal_keeps_its_error_kind() {
        let e = anyhow::Error::new(SandboxRefused(SandboxError::MissingBwrap));
        let resp = run_error(e);
        assert_eq!(
            resp.as_error().map(|(k, _)| k),
            Some(ErrorKind::SandboxUnavailable)
        );
    }

    #[test]
    fn a_sandbox_refusal_keeps_its_remedy_through_the_error_chain() {
        let e = anyhow::Error::new(SandboxRefused(SandboxError::TiocstiEnabled));
        let resp = run_error(e);
        let (_, message) = resp.as_error().expect("error");
        assert!(message.contains("unrestricted"), "{message}");
    }

    #[test]
    fn a_policy_refusal_is_not_reported_as_a_sandbox_problem() {
        // The remedy differs. `SandboxUnavailable` means "re-run with
        // --sandbox unrestricted", which for a confined break-glass request
        // is the opposite of what the user should do.
        let e = anyhow::Error::new(PolicyRefused(PolicyError::BreakGlassCannotBeConfined(
            rime_agent_core::protocol::SandboxPolicy::Project,
        )));
        let resp = run_error(e);
        assert_eq!(resp.as_error().map(|(k, _)| k), Some(ErrorKind::PolicyRefused));
        let (_, message) = resp.as_error().expect("error");
        assert!(message.contains("no_new_privs"), "{message}");
    }

    #[test]
    fn a_refused_grant_is_a_permission_answer_and_a_refused_ttl_is_not() {
        // The two failures a `--unsafe-everything` run can hit, and a client
        // branches on the kind: `PermissionDenied` means somebody has to
        // authorise this, `PolicyRefused` means the request itself was out of
        // bounds and no amount of authorising will help.
        let denied = run_error(anyhow::Error::new(GrantRefused(
            "this connection belongs to session 3".into(),
        )));
        assert_eq!(
            denied.as_error().map(|(k, _)| k),
            Some(ErrorKind::PermissionDenied)
        );

        let ttl = run_error(anyhow::Error::new(TtlRefused(
            "break-glass caps at 1h".into(),
        )));
        assert_eq!(ttl.as_error().map(|(k, _)| k), Some(ErrorKind::PolicyRefused));
    }

    #[test]
    fn a_session_with_no_narrowing_gets_the_runtime_allowlist_unchanged() {
        // The behaviour every allowlisted session had before `allow` existed,
        // asserted so that adding the field cannot quietly change it. A
        // regression here is a machine where every existing capsule suddenly
        // reaches nothing.
        let runtime = Allowlist::parse(&["api.example.com", "files.example.com"]).expect("parse");
        let got = session_allowlist(&runtime, None, NetworkPolicy::Allowlist).expect("unchanged");
        assert_eq!(got.lines(), runtime.lines());
        // And on a session with no allowlist at all, where the value is never
        // enforced: still not an error, because nothing was asked for.
        assert!(session_allowlist(&runtime, None, NetworkPolicy::Open).is_ok());
    }

    #[test]
    fn a_narrowed_session_is_confined_to_what_it_named_and_cannot_widen() {
        let runtime =
            Allowlist::parse(&["api.example.com", "files.example.com", "other.test:8443"])
                .expect("parse");
        let named = ["api.example.com".to_string()];
        let got =
            session_allowlist(&runtime, Some(&named), NetworkPolicy::Allowlist).expect("narrow");
        assert_eq!(got.lines(), vec!["api.example.com"]);
        // The half that is the boundary rather than the bookkeeping: a
        // destination the RUNTIME allows is refused for this session. Asserted
        // through `decide`, which is the question the egress proxy asks, and
        // not by counting the lines — a list of the right length pointing at
        // the wrong hosts would pass that.
        use rime_agent_core::destination::Destination;
        assert!(got
            .decide(&Destination::parse("api.example.com:443").expect("dest"))
            .is_allowed());
        assert!(!got
            .decide(&Destination::parse("files.example.com:443").expect("dest"))
            .is_allowed());

        // And it can only subtract. A destination the runtime does not carry
        // is refused rather than added, with the line that would add it.
        let wider = ["evil.example.com".to_string()];
        let e = session_allowlist(&runtime, Some(&wider), NetworkPolicy::Allowlist)
            .expect_err("a widening");
        assert!(e.to_string().contains("evil.example.com"), "{e}");
        assert!(e.to_string().contains("rime agent allow"), "{e}");
    }

    #[test]
    fn naming_destinations_for_a_session_with_no_allowlist_is_refused_not_ignored() {
        // Three networks, because the failure differs in direction and the
        // refusal has to cover both: `open` reaches everything the machine
        // does and `offline`/`brokered` reach nothing through this list, so a
        // narrowing on any of them is a boundary the caller believes they
        // asked for and did not get.
        let runtime = Allowlist::parse(&["api.example.com"]).expect("parse");
        let named = ["api.example.com".to_string()];
        for network in [
            NetworkPolicy::Open,
            NetworkPolicy::Offline,
            NetworkPolicy::Brokered,
        ] {
            let e = match session_allowlist(&runtime, Some(&named), network) {
                Err(e) => e,
                Ok(list) => panic!(
                    "`{}` accepted a session allowlist it will never enforce: {:?}",
                    network.as_str(),
                    list.lines()
                ),
            };
            // The refusal names the network it refused, because the remedy
            // depends on which one it was: the caller either wanted the
            // allowlist mode or did not want the flag.
            let text = e.to_string();
            assert!(text.contains(network.as_str()), "{text}");
            assert!(text.contains("--network allowlist"), "{text}");
        }
        // And the one network where it is honoured, so the loop above cannot
        // pass by refusing everything.
        assert!(session_allowlist(&runtime, Some(&named), NetworkPolicy::Allowlist).is_ok());
    }

    #[test]
    fn a_refused_narrowing_is_a_policy_answer_and_not_a_malformed_request() {
        // P2-012. The default arm of `run_error` is `BadRequest`, so a
        // refusal with no arm of its own reads to the caller as "your command
        // line was wrong" — and the remedy for this one is on the MACHINE
        // (`rime agent allow`), not in the command they typed. The two are
        // asserted together because the only way this assertion can fail is by
        // the arm being deleted, and then it falls through to `BadRequest`.
        let refused = run_error(anyhow::Error::new(AllowlistRefused(
            "'evil.example' is not covered by the runtime's allowlist".into(),
        )));
        assert_eq!(
            refused.as_error().map(|(k, _)| k),
            Some(ErrorKind::PolicyRefused)
        );
        // And the message survives the mapping, because it is the half that
        // names the destination and the command that would permit it.
        let (_, message) = refused.as_error().expect("error");
        assert!(message.contains("evil.example"), "{message}");
    }

    #[test]
    fn an_ordinary_failure_is_a_bad_request_not_a_sandbox_problem() {
        let e = anyhow::anyhow!("working directory /nope does not exist");
        let resp = run_error(e);
        assert_eq!(resp.as_error().map(|(k, _)| k), Some(ErrorKind::BadRequest));
    }

    /// A real session on a real PTY, running a shell that reads one line and
    /// says what it got.
    ///
    /// Deliberately not a mock and not a socket pair. What `Request::Input`
    /// has to be right about is the LINE DISCIPLINE — whether the byte it
    /// appends for `--submit` is the byte that ends a line — and a socket
    /// carries every byte equally, so it would prove the plumbing and hide the
    /// only interesting question. `pty::spawn` is the same call a session is
    /// started with, so the terminal modes are the shipped ones.
    fn a_session_reading_one_line() -> Option<(registry::Handle, pty::Spawned, PathBuf)> {
        let script = "read line; echo \"got:[$line]\"; sleep 30";
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
        let spawned = pty::spawn(
            &argv,
            std::path::Path::new("/tmp"),
            &[],
            false,
            true,
            rime_agent_core::term::WinSize { cols: 80, rows: 24 },
        )
        .ok()?;

        let dir = std::env::temp_dir().join(format!(
            "rime-agentd-input-{}-{}",
            std::process::id(),
            spawned.pid
        ));
        let mut reg = registry::Registry::with_store(dir.clone());
        let mut info = sample_live_info(1);
        info.pid = spawned.pid;
        let handle = reg.insert(info, spawned.master, spawned.pid, spawned.pgid);
        Some((handle, spawned, dir))
    }

    fn sample_live_info(id: u32) -> rime_agent_core::protocol::SessionInfo {
        use rime_agent_core::protocol::{AgentState, SessionInfo};
        SessionInfo {
            id,
            agent: "generic".into(),
            program: "sh".into(),
            args: vec![],
            cwd: "/tmp".into(),
            project: None,
            project_name: None,
            name: None,
            title: None,
            worktree: None,
            state: AgentState::Working,
            detail: None,
            paused: false,
            policy: rime_agent_core::AgentPolicy::default(),
            allowlist: None,
            request_origin: Some(rime_agent_core::policy::RequestOrigin::LocalTerminal),
            origin_source: Some(rime_agent_core::origin::OriginSource::Observed),
            grant: None,
            grant_expires_ms: None,
            native_observed: None,
            pid: 0,
            started: 0,
            last_activity: 0,
            exit_code: None,
            exit_signal: None,
            checkpoint: None,
            cols: 80,
            rows: 24,
            attached: 0,
            actor: None,
            telemetry: None,
            children: vec![],
            injected: 0,
            capsule: None,
        }
    }

    /// Drain the master for up to `ms`, stopping early once `marker` is seen.
    fn read_until(master: RawFd, marker: &str, ms: u64) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            if !pty::wait_readable(master, 50) {
                continue;
            }
            let mut buf = [0u8; 4096];
            match pty::read_nonblocking(master, &mut buf) {
                Ok(Some(n)) if n > 0 => seen.extend_from_slice(&buf[..n]),
                Ok(_) => {}
                Err(_) => break,
            }
            if String::from_utf8_lossy(&seen).contains(marker) {
                break;
            }
        }
        String::from_utf8_lossy(&seen).to_string()
    }

    #[test]
    fn text_written_into_a_session_arrives_and_only_submit_ends_the_line() {
        let Some((handle, spawned, dir)) = a_session_reading_one_line() else {
            // No PTY available (a container without /dev/pts). Skipping is
            // correct here and a false pass is not, so it says so.
            eprintln!("skipping: pty::spawn failed on this machine");
            return;
        };

        // Let the shell reach `read` before anything is typed, or the bytes
        // land before there is a reader and the test proves the timing rather
        // than the write.
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Phase 1: the words, with no terminator. This is what `rime agent
        // input` does WITHOUT --submit, and what the shell's push-to-talk
        // route does today.
        match write_input(&handle, b"run the tests") {
            Input::Written => {}
            Input::Exited => panic!("the session was reported as exited"),
            Input::Failed(e) => panic!("the write failed: {e}"),
        }
        let echoed = read_until(spawned.master, "run the tests", 2000);
        assert!(
            echoed.contains("run the tests"),
            "the text never reached the terminal: {echoed:?}"
        );
        assert!(
            !echoed.contains("got:["),
            "the line was submitted without --submit: {echoed:?}"
        );

        // Phase 2: the carriage return alone. What this proves is that the
        // terminator is what turns written bytes into a line the agent acts
        // on, which is the property `--submit` sells. What it does NOT prove
        // is that CR is the only byte that would: measured on this machine,
        // CR and LF both end the line here, because ICRNL is on by default in
        // cooked mode. The CR is chosen for raw-mode TUIs, and that case is
        // out of reach of this fixture. Said plainly rather than left to be
        // inferred from a passing assertion.
        match write_input(&handle, b"\r") {
            Input::Written => {}
            other => panic!(
                "the submit write failed: {}",
                match other {
                    Input::Failed(e) => e,
                    _ => "session reported exited".to_string(),
                }
            ),
        }
        let after = read_until(spawned.master, "got:[", 3000);
        assert!(
            after.contains("got:[run the tests]"),
            "the carriage return did not end the line: {after:?}"
        );

        // Tidy: the fixture sleeps 30s, so it is killed rather than waited on.
        pty::signal_group(spawned.pgid, libc::SIGKILL).ok();
        pty::close(spawned.master);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_exited_session_is_reported_as_exited_and_not_as_a_broken_terminal() {
        // The two failures a caller branches on differently: "pick another
        // target" against "the terminal is broken". A dead session that came
        // back as an I/O error would send the shell's push-to-talk route
        // looking for a fault in the PTY layer.
        let dir = std::env::temp_dir().join(format!("rime-agentd-input-dead-{}", std::process::id()));
        let mut reg = registry::Registry::with_store(dir.clone());
        let mut info = sample_live_info(2);
        info.exit_code = Some(0);
        assert!(!info.is_live());
        // A VALID descriptor, so a write would genuinely succeed if the live
        // check were dropped. With -1 here the test would pass on the fd guard
        // and prove nothing about the state check.
        let (a, _b) = UnixStream::pair().unwrap();
        let handle = reg.insert(info, a.as_raw_fd(), 0, 0);
        assert!(matches!(write_input(&handle, b"x"), Input::Exited));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_live_session_whose_master_is_already_closed_is_exited_too() {
        // The reaper sets `master` to -1 as the child goes away, so a session
        // can be marked live for the moment between the two. Writing to -1
        // would be EBADF surfaced as `Internal`, which reads as a bug in the
        // runtime rather than as a session that has gone.
        let dir = std::env::temp_dir().join(format!("rime-agentd-input-fd-{}", std::process::id()));
        let mut reg = registry::Registry::with_store(dir.clone());
        let handle = reg.insert(sample_live_info(3), -1, 0, 0);
        assert!(matches!(write_input(&handle, b"x"), Input::Exited));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_cloned_socket_is_recognised_as_the_same_peer() {
        use std::os::unix::io::AsRawFd;
        let (a, _b) = UnixStream::pair().unwrap();
        let clone = a.try_clone().unwrap();
        assert_ne!(a.as_raw_fd(), clone.as_raw_fd(), "expected a new descriptor");
        assert!(same_peer(a.as_raw_fd(), clone.as_raw_fd()));

        let (c, _d) = UnixStream::pair().unwrap();
        assert!(!same_peer(a.as_raw_fd(), c.as_raw_fd()));
    }

    /// P1-012's first criterion, at the only place that can deliver it.
    ///
    /// There is an `/etc/profile.d` drop-in that puts the tool shims on PATH,
    /// and it is NOT what makes this work for an agent: profile.d is read by a
    /// login shell, and an agent's tool calls are `bash -c '…'`. The session's
    /// `bin` directory is what goes first on its `PATH`, so this is where a
    /// skill's own `wrangler deploy` either finds the broker or does not.
    ///
    /// Mutation: drop the `install_tool_shims` call from `install_git_shim`.
    /// Red.
    #[test]
    fn a_session_gets_the_tool_shims_on_the_path_its_own_commands_use() {
        let root = std::env::temp_dir().join(format!(
            "rime-toolshim-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("image");
        std::fs::create_dir_all(&source).expect("source");
        for tool in ["wrangler", "terraform"] {
            std::fs::write(source.join(tool), "#!/bin/sh\nexit 0\n").expect("shim");
        }
        // A test that installed from an empty directory would pass without
        // linking anything, so the fixture has to be real first.
        assert!(source.join("wrangler").exists());

        // The REAL wiring, not the installer on its own: what a session gets
        // is whatever ends up in the one directory that goes first on its
        // PATH, and a test that called each installer separately would prove
        // each works rather than that either is reached.
        let scratch = root.join("scratch");
        std::fs::create_dir_all(&scratch).expect("scratch");
        let rime = root.join("rime");
        std::fs::write(&rime, "#!/bin/sh\nexit 0\n").expect("rime");
        let bin = install_session_bin(&scratch, &rime, &source).expect("a session bin");

        // git first, because that is what this directory has always been for
        // and a regression there would be the louder failure.
        assert!(bin.join("git").exists(), "the git shim is gone");

        for tool in ["wrangler", "terraform"] {
            let link = bin.join(tool);
            assert!(
                link.exists(),
                "{tool} is not on the session's own PATH, so a skill's \
                 `{tool} deploy` reaches the real tool with no credential"
            );
            assert_eq!(
                std::fs::read_link(&link).expect("a link"),
                source.join(tool),
                "{tool} does not point at the shim"
            );
        }

        // Installing again is not an error: a session is set up once, but a
        // link left behind by anything else must not stop this.
        install_session_bin(&scratch, &rime, &source).expect("again");
        assert!(bin.join("wrangler").exists());

        // An image that never installed them leaves nothing behind and does
        // not fail — a development build running from a checkout has no
        // /usr/libexec/rime, and a session must still start.
        let empty = root.join("no-image");
        let bare_scratch = root.join("bare");
        std::fs::create_dir_all(&bare_scratch).expect("bare");
        let bare = install_session_bin(&bare_scratch, &rime, &empty).expect("still a bin");
        assert!(!bare.join("wrangler").exists());
        assert!(bare.join("git").exists(), "git must still be brokered");

        std::fs::remove_dir_all(&root).ok();
    }

    /// The constant the image installs to and the one a session links from are
    /// the same string, and the Containerfile is the other half of it.
    #[test]
    fn the_tool_shim_directory_is_the_one_the_image_writes() {
        assert_eq!(TOOL_SHIM_DIR, "/usr/libexec/rime/tools");
        assert!(Path::new(TOOL_SHIM_DIR).is_absolute());
    }

    // ── docs/remote-live-contract.md §1.2, §1.3, §1.7 ───────────────────────

    /// Every read the program's side of a PTY got for `ms`, with when it got
    /// it. The slave is raw and non-blocking (`pty::bare_pair`), so a read
    /// returns whatever the line discipline has, the moment it has it.
    fn reads_on(slave: RawFd, ms: u64) -> Vec<(std::time::Instant, Vec<u8>)> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        let mut out = Vec::new();
        while std::time::Instant::now() < deadline {
            if !pty::wait_readable(slave, 5) {
                continue;
            }
            let mut buf = [0u8; 4096];
            if let Ok(Some(n)) = pty::read_nonblocking(slave, &mut buf) {
                if n > 0 {
                    out.push((std::time::Instant::now(), buf[..n].to_vec()));
                }
            }
        }
        out
    }

    /// Reads that arrived within `gap` of each other, joined: what a program
    /// that reads as fast as it can would have seen as one burst. Joined
    /// rather than compared read for read, because the kernel may hand one
    /// write over in two reads and that is not what is being measured.
    fn bursts(
        reads: &[(std::time::Instant, Vec<u8>)],
        gap: std::time::Duration,
    ) -> Vec<(std::time::Instant, std::time::Instant, Vec<u8>)> {
        let mut out: Vec<(std::time::Instant, std::time::Instant, Vec<u8>)> = Vec::new();
        for (at, bytes) in reads {
            match out.last_mut() {
                Some((_, last, acc)) if at.duration_since(*last) < gap => {
                    acc.extend_from_slice(bytes);
                    *last = *at;
                }
                _ => out.push((*at, *at, bytes.clone())),
            }
        }
        out
    }

    /// The property `submit` exists for, as the program on the PTY sees it:
    /// the text as one burst, then Enter as a burst of its own at least 50 ms
    /// later — 50 ms being the smallest gap measured to submit in Claude Code.
    fn is_text_then_a_separate_enter(
        got: &[(std::time::Instant, std::time::Instant, Vec<u8>)],
        text: &[u8],
    ) -> bool {
        got.len() == 2
            && got[0].2 == text
            && got[1].2 == b"\r"
            && got[1].0.duration_since(got[0].1) >= std::time::Duration::from_millis(50)
    }

    #[test]
    fn submit_writes_enter_as_its_own_read_a_gap_after_the_text() {
        let Some((master, slave)) = pty::bare_pair() else {
            eprintln!("SKIP: no /dev/ptmx on this machine");
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "rime-agentd-submit-{}-{master}",
            std::process::id()
        ));
        let mut reg = registry::Registry::with_store(dir.clone());
        let handle = reg.insert(sample_live_info(5), master, 0, 0);
        // The size that was measured to be read as a paste.
        let text = "a".repeat(250);

        let reader = std::thread::spawn(move || reads_on(slave, 600));
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(matches!(submit_input(&handle, text.as_bytes()), Input::Written));
        let got = bursts(&reader.join().expect("reader"), std::time::Duration::from_millis(30));
        assert!(
            is_text_then_a_separate_enter(&got, text.as_bytes()),
            "expected the text, then a lone CR at least 50 ms later; got {:?}",
            got.iter()
                .map(|(a, b, bytes)| (b.duration_since(*a), String::from_utf8_lossy(bytes).len()))
                .collect::<Vec<_>>()
        );

        // The negative control, on the same terminal: the text and the CR in
        // ONE write — what `rime agent input --submit` did before the flag,
        // and what a phone did. The same predicate has to fail, or the test
        // above would pass for a gap nobody put there.
        let reader = std::thread::spawn(move || reads_on(slave, 400));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut one = text.clone().into_bytes();
        one.push(b'\r');
        assert!(matches!(write_input(&handle, &one), Input::Written));
        let got = bursts(&reader.join().expect("reader"), std::time::Duration::from_millis(30));
        assert!(
            !is_text_then_a_separate_enter(&got, text.as_bytes()),
            "a single write must not look like a submit"
        );
        assert_eq!(got.len(), 1, "one write is one burst");
        assert_eq!(got[0].2, one);

        pty::close(slave);
        pty::close(master);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn submit_with_no_text_is_enter_alone() {
        let Some((master, slave)) = pty::bare_pair() else {
            eprintln!("SKIP: no /dev/ptmx on this machine");
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "rime-agentd-submit-empty-{}-{master}",
            std::process::id()
        ));
        let mut reg = registry::Registry::with_store(dir.clone());
        let handle = reg.insert(sample_live_info(6), master, 0, 0);
        let start = std::time::Instant::now();
        assert!(matches!(submit_input(&handle, b""), Input::Written));
        assert!(
            start.elapsed() < logic::SUBMIT_GAP,
            "an empty submit has no burst to wait behind"
        );
        let got = bursts(&reads_on(slave, 200), std::time::Duration::from_millis(30));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].2, b"\r");
        pty::close(slave);
        pty::close(master);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn submit_to_an_exited_session_types_nothing_and_says_so() {
        let dir = std::env::temp_dir().join(format!("rime-agentd-submit-dead-{}", std::process::id()));
        let mut reg = registry::Registry::with_store(dir.clone());
        let mut info = sample_live_info(7);
        info.exit_code = Some(0);
        let (a, _b) = UnixStream::pair().unwrap();
        let handle = reg.insert(info, a.as_raw_fd(), 0, 0);
        let start = std::time::Instant::now();
        assert!(matches!(submit_input(&handle, b"x"), Input::Exited));
        assert!(start.elapsed() < logic::SUBMIT_GAP, "no gap is slept for a write that never happened");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_a_changed_title_is_worth_writing_down() {
        use rime_agent_core::session::OutputScanner;
        let mut info = sample_live_info(8);
        let mut scanner = OutputScanner::new();
        // The first title is a change.
        let signals = scanner.feed("\x1b]0;✳ Rime showcase studio\x07".as_bytes());
        assert!(retitle(&mut info, &signals));
        assert_eq!(info.title.as_deref(), Some("Rime showcase studio"));
        // Claude's spinner cycling while it works: the same title every frame,
        // so not one of them writes the record.
        for frame in ["✢", "✳", "✶", "✻", "✽", "·"] {
            let raw = format!("\x1b]0;{frame} Rime showcase studio\x07");
            assert!(!retitle(&mut info, &scanner.feed(raw.as_bytes())), "{frame}");
        }
        // Output with no title in it leaves the title alone.
        assert!(!retitle(&mut info, &scanner.feed(b"plain output\r\n")));
        assert_eq!(info.title.as_deref(), Some("Rime showcase studio"));
        // The last title in a read is the one kept.
        let signals = scanner.feed(b"\x1b]0;first\x07\x1b]2;second\x07");
        assert!(retitle(&mut info, &signals));
        assert_eq!(info.title.as_deref(), Some("second"));
        // A cleared title clears the field.
        assert!(retitle(&mut info, &scanner.feed(b"\x1b]0;\x07")));
        assert_eq!(info.title, None);
        // And a name is never touched by any of it: only a person sets that.
        assert_eq!(info.name, None);
    }

    #[test]
    fn a_phone_detaching_gives_the_desktop_its_size_back() {
        use rime_agent_core::session::Viewer;
        use rime_agent_core::term::{window_size, WinSize};
        let Some((master, slave)) = pty::bare_pair() else {
            eprintln!("SKIP: no /dev/ptmx on this machine");
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "rime-agentd-size-{}-{master}",
            std::process::id()
        ));
        let mut reg = registry::Registry::with_store(dir.clone());
        let handle = reg.insert(sample_live_info(9), master, 0, 0);
        let desk = WinSize { cols: 180, rows: 50 };
        let phone = WinSize { cols: 46, rows: 30 };

        let (local, _local_peer) = UnixStream::pair().unwrap();
        let (remote, _remote_peer) = UnixStream::pair().unwrap();
        {
            let mut s = handle.lock().unwrap();
            s.attach_size(Viewer::Local, desk).unwrap();
            s.attach_as(local.try_clone().unwrap(), 0, Viewer::Local).unwrap();
            s.attach_size(Viewer::Remote, phone).unwrap();
            s.attach_as(remote.try_clone().unwrap(), 0, Viewer::Remote).unwrap();
        }
        assert_eq!(window_size(master), phone, "the phone is drawn at the phone's size");

        // The phone's own detach — the path `handle_attach` takes when the
        // phone closes its terminal.
        detach(&handle, &remote);
        assert_eq!(window_size(master), desk, "the desktop did not get its size back");
        {
            let s = handle.lock().unwrap();
            assert_eq!((s.info.cols, s.info.rows), (desk.cols, desk.rows));
            assert_eq!(s.info.attached, 1);
        }

        // A late resize from the phone, on its own connection, after it left.
        let applied = handle
            .lock()
            .unwrap()
            .resize_from(Viewer::Remote, WinSize { cols: 40, rows: 20 })
            .unwrap();
        assert!(!applied);
        assert_eq!(window_size(master), desk, "a stray phone resize shrank the desktop again");

        // And the desktop leaving changes nothing: there is no phone to give
        // anything back from.
        detach(&handle, &local);
        assert_eq!(window_size(master), desk);

        pty::close(slave);
        pty::close(master);
        std::fs::remove_dir_all(&dir).ok();
    }
}
