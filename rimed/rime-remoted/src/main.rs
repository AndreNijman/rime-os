//! `rime-remoted` — the desktop half of Rime Remote (roadmap §46, P1-050).
//!
//! Per-user, unprivileged, and deliberately a second daemon rather than a
//! feature of `rime-agentd`. Three reasons, in order of how much they matter:
//!
//! * **It is the only thing here that listens on a network.** `rime-agentd`
//!   binds a Unix socket in a 0700 directory and nothing else; giving it a TCP
//!   listener would put an attack surface on the process that owns every
//!   agent PTY on the machine. A separate process can be stopped, can fail,
//!   and can be compromised without taking the agent runtime with it.
//! * **It has a different lifetime.** `rime agent enable` starts the runtime
//!   for somebody who is going to run an agent locally, and most of them never
//!   pair a phone. `rime remote enable` is a separate decision.
//! * **It is a client of the runtime, not a part of it.** Everything it does
//!   goes through the same control socket `rime` uses, under an origin the
//!   daemon assigns. If this process is taken over, what the attacker gets is
//!   a `claude-remote-control` origin — no root approvals, no break-glass, no
//!   raw secrets — and not the runtime's own privileges.
//!
//! ## What it will not do
//!
//! Start unless this process is a systemd user service. A proxy started from
//! a login session is classified `local-terminal` by everything it connects
//! to, and §7 reserves approving a root operation for exactly that. The
//! per-connection origin latch in `rime-agentd` closes the hole properly;
//! this refuses to rely on one guard for it. `--allow-foreground` is there
//! for a developer running it by hand, and it says what it is giving up.

mod agents;
mod calls;
mod clipboard;
mod companion;
mod computers;
mod control;
mod dialer;
mod discovery;
mod intro;
mod machine;
mod media;
mod messages;
mod mirror;
mod net;
mod nex;
mod peer;
mod phone;
mod photos;
mod presence;
mod proxy;
mod push;
mod relay;
mod serve;
mod share;
mod files;
mod handoff;
mod state;

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use rime_remote_core::identity::Identity;

use crate::state::State;

/// The port the LAN listener binds.
///
/// Registered in `files/system/firewall/services` as `rime-remote`, and
/// **closed by default** like everything else: the machine ships default-drop
/// and a user opens it with `rime firewall allow rime-remote`. That is a
/// deliberate cost. The alternative — opening a port because the feature
/// exists — is how a default-drop policy becomes a list of things somebody
/// once wanted.
///
/// The relay path needs no exception at all, because it is an outbound
/// connection. On a network where the owner has not opened the port, Rime
/// Remote still works through the relay; on their own LAN, opening it buys a
/// faster path that involves nobody else.
pub const DEFAULT_PORT: u16 = 7717;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }
    if let Err(e) = run(&args) {
        eprintln!("rime-remoted: {e}");
        std::process::exit(1);
    }
}

const USAGE: &str = "\
rime-remoted — the desktop service for Rime Remote

  --port <n>          listen on this TCP port (default 7717)
  --handshake-timeout-ms <n>
                      how long an unauthenticated peer may take (default
                      30000, clamped to 1000..120000). Lower it in a test;
                      raising it past two minutes is refused, because the
                      deadline is what stops an idle connection holding a
                      thread on the only network listener in the stack.
  --relay <url>       the rendezvous to fall back to when no LAN path works
  --ping-interval-ms  how often an open connection is measured (default 15000);
                      three unanswered in a row close it
  --nexd <path>       nex's socket (default $XDG_RUNTIME_DIR/nex/nexd.sock);
                      paired devices reach nex through this service only
  --no-announce       do not advertise this machine over mDNS on this network
  --allow-foreground  run outside a systemd user unit (see below)
  --help

This service is normally started by systemd:

  rime remote enable

It refuses to run from a login session, because `rime-agentd` would then
classify every request it forwards as a human at this machine. Pass
--allow-foreground to override that for development; the requests it forwards
are still recorded as claude-remote-control, because it declares that on every
connection, but the second guard is gone.
";

/// How many ports after [`DEFAULT_PORT`] a daemon with no `--port` tries.
///
/// One listener per port, and one daemon per account: a second person on the
/// same machine — or root's lingering user manager, which is what took 7717
/// on the L16 on 2026-09-23 and left the owner's daemon crash-looping with
/// "Address already in use" — must not stop this one starting. The port a
/// daemon actually gets is read back and advertised, so a phone follows it;
/// only the LAN firewall rule names 7717, and the relay needs no port at all.
const FALLBACK_PORTS: u16 = 15;

/// Bind the listener: dual-stack first, IPv4 as the fallback. With no
/// `--port`, a port in use moves on to the next one instead of failing; an
/// explicit `--port` is honoured exactly.
fn listen_on(port: u16, may_move: bool) -> Result<TcpListener, String> {
    let bind = |p: u16| TcpListener::bind(("::", p)).or_else(|_| TcpListener::bind(("0.0.0.0", p)));
    let last = if may_move { port.saturating_add(FALLBACK_PORTS) } else { port };
    let mut first_err = None;
    for p in port..=last {
        match bind(p) {
            Ok(l) => {
                if p != port {
                    eprintln!(
                        "rime-remoted: port {port} is in use (another account's Rime Remote?), \
                         listening on {p} instead; pairing codes carry {p}"
                    );
                }
                return Ok(l);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && may_move => {
                first_err.get_or_insert(e);
            }
            Err(e) => return Err(format!("cannot listen on port {p}: {e}")),
        }
    }
    Err(format!(
        "cannot listen on port {port} or the {FALLBACK_PORTS} after it: {}",
        first_err.map(|e| e.to_string()).unwrap_or_default()
    ))
}

fn run(args: &[String]) -> Result<(), String> {
    let asked_port = flag(args, "--port")
        .map(|v| v.parse::<u16>().map_err(|_| format!("--port {v} is not a port")))
        .transpose()?;
    let port = asked_port.unwrap_or(DEFAULT_PORT);
    let relay = flag(args, "--relay");
    let handshake = match flag(args, "--handshake-timeout-ms") {
        None => serve::HANDSHAKE_TIMEOUT,
        Some(v) => {
            let ms: u64 = v
                .parse()
                .map_err(|_| format!("--handshake-timeout-ms {v} is not a number"))?;
            // Clamped rather than trusted. This deadline is a denial-of-service
            // guard, and a flag that could switch it off would be a way to ask
            // for the bug it exists to prevent.
            std::time::Duration::from_millis(ms.clamp(1_000, 120_000))
        }
    };
    // How often an open connection is measured. A flag for the same reason
    // --handshake-timeout-ms is one: the shipped value is right for a phone
    // and wrong for a suite that has to watch a measurement happen. Clamped,
    // so it cannot be turned into a flood a device pays for.
    let ping_interval = match flag(args, "--ping-interval-ms") {
        None => serve::PING_INTERVAL,
        Some(v) => {
            let ms: u64 = v
                .parse()
                .map_err(|_| format!("--ping-interval-ms {v} is not a number"))?;
            std::time::Duration::from_millis(ms.clamp(50, 600_000))
        }
    };
    let allow_foreground = args.iter().any(|a| a == "--allow-foreground");

    guard_placement(allow_foreground)?;

    let state_home = rime_agent_core::paths::state_home();
    let identity = Identity::load_or_create(&Identity::path_in(&state_home))
        .map_err(|e| format!("this machine has no remote identity: {e}"))?;
    let store_path = rime_remote_core::device::DeviceStore::path_in(&state_home);
    let machine = machine_name();

    // Dual-stack, with IPv4 as the fallback and not as the intention.
    //
    // The regression this waited for has been measured and closed: a `::`
    // listener hands every IPv4 peer to `accept(2)` as `::ffff:a.b.c.d`, and
    // `Ipv6Addr::is_loopback` is false for `::ffff:127.0.0.1` — it answers
    // only for `::1` — so `State::path_of` would have recorded every relayed
    // session as a LAN one and told the owner nobody else was on the path.
    // `state::canonical` flattens the address once where `serve.rs` reads it,
    // and `arrived_by_relay` canonicalises again where the decision is made;
    // `a_v4_mapped_relay_splice_is_a_relay_session` fails without either.
    //
    // `reachable_on` already knew what to do with a `::` listener, so the
    // pairing code widens to this machine's IPv6 addresses by itself — which
    // is the point: on an IPv6-only network a phone could not reach this
    // daemon at all, and the firewall needs no change because `rime-firewall`'s
    // table is `inet`, which is both families.
    //
    // The fallback is not decoration. A machine booted with `ipv6.disable=1`
    // has no `AF_INET6` at all and `bind("[::]")` fails outright there; a
    // daemon that refused to start on one would be a regression far worse than
    // the one above. `net.ipv6.bindv6only` is 0 by default on Linux and this
    // does not set `IPV6_V6ONLY`, so a `::` bind accepts both families; on a
    // system where somebody has set it to 1 the IPv4 half is lost, which is
    // why the address actually bound is read back below rather than assumed.
    let listener = listen_on(port, asked_port.is_none())?;
    // The ADDRESS AND THE PORT the listener actually got, not the ones asked
    // for. They differ whenever `--port 0` is used — the kernel picks one, and
    // the daemon then advertised `192.168.1.232:0` in the pairing code, told
    // mDNS port 0, and dialled 127.0.0.1:0 for every relay splice. Measured on
    // this tree before the change; every one of those three is a fact about
    // the listener and every one of them read the request instead.
    let local = listener
        .local_addr()
        .map_err(|e| format!("the listener has no address: {e}"))?;
    let bound = local.ip();
    let port = local.port();
    let state = State::new(identity, machine, port, bound, relay, store_path, ping_interval)
        .map_err(|e| format!("the paired-device store is unusable: {e}"))?;
    if let Some(p) = flag(args, "--nexd") {
        state.set_nexd(PathBuf::from(p));
    }
    // A phone's Do Not Disturb, for the Shell to follow (contract §2.9).
    // Every harness gives this daemon a private XDG_RUNTIME_DIR, so a test
    // never writes where the real Shell is watching.
    state.set_focus_file(phone::focus_file_in(&rime_agent_core::paths::runtime_dir()));
    // A phone's photo thumbnails, for the Shell's grid (contract §2.13). This
    // removes whatever an earlier run of this process left there.
    state.photos.set_dir(photos::dir_in(&rime_agent_core::paths::runtime_dir()));

    // A phone notification the person dismissed here is cancelled on the
    // phone, if the phone said it does that; otherwise it stays there.
    {
        let state = Arc::clone(&state);
        mirror::mirror().on_dismissed(Box::new(move |device, key| {
            if state.phones().get(device).is_some_and(|r| r.can_dismiss()) {
                state.events.send(device, &serde_json::json!({"event": mirror::DISMISSED_EVENT, "key": key}));
            }
        }));
    }
    {
        // Only to a phone whose person turned replying on: the phone sends
        // it as them, through the app's own reply action.
        let state = Arc::clone(&state);
        // Not sent (no connection, or replying turned off since the card was
        // drawn): the mirror says so here and gives the words back.
        mirror::mirror().on_replied(Box::new(move |device, key, text| {
            match state.phones().get(device).map(|r| r.can_reply()) {
                Some(true) => {}
                Some(false) => return Err(mirror::NotSent::Off),
                None => return Err(mirror::NotSent::Offline),
            }
            let line = serde_json::json!({"event": mirror::REPLY_EVENT, "key": key, "text": text});
            if state.events.send(device, &line) { Ok(()) } else { Err(mirror::NotSent::Offline) }
        }));
    }

    {
        // A card closed (dismissed, expired, or gone with its server): the
        // link it held goes with it (handoff.rs).
        let state = Arc::clone(&state);
        mirror::mirror().on_own_gone(Box::new(move |_device, key| handoff::on_gone(&state, key)));
    }

    {
        // Answer or Decline pressed on a ringing call's notification (contract
        // §2.12). Heard only from the notification server's current bus owner,
        // the same trust as an inline reply (§2.7): whoever draws the desktop's
        // notifications is the person at it. On a thread of its own, since the
        // phone's `call_done` takes a round trip and the mirror's worker must
        // not wait on it. What did not happen is said in a notice.
        let state = Arc::clone(&state);
        mirror::mirror().on_action(Box::new(move |device, key, action| {
            // Remove on a "joined" notification (intro.rs): it only takes away.
            if intro::on_action(&state, device, key, action) {
                return;
            }
            // Show in folder on a file another computer sent (files.rs).
            if files::on_action(&state, key, action) {
                return;
            }
            // Open on a link another computer offered (handoff.rs): the only
            // way a link is ever opened.
            if handoff::on_action(&state, device, key, action) {
                return;
            }
            if key != calls::CALL_KEY || !matches!(action, "answer" | "decline") {
                return;
            }
            let (state, device, action) = (Arc::clone(&state), device.to_string(), action.to_string());
            std::thread::spawn(move || {
                let name = state
                    .devices()
                    .ok()
                    .and_then(|s| s.find(&device).ok().map(|d| d.name.clone()))
                    .unwrap_or_else(|| "Your phone".into());
                let outcome = match state.phones().get(&device) {
                    Some(report) => state.calls.act(&state.events, &device, report, &action, None, calls::ACTION_WAIT),
                    None => Err(calls::Refusal::NotConnected),
                };
                if let Err(why) = outcome {
                    mirror::mirror().submit(mirror::Job::Notice(calls::not_done(&action, &why, &name)));
                }
            });
        }));
    }

    {
        // Play, pause, skip and seek on a phone's player (contract §2.8): one
        // line on that phone's events channel. Only a player whose report
        // carried `can` takes a method at all (media.rs decides); a phone
        // with no channel open makes the method an error, not a success.
        let state = Arc::clone(&state);
        media::media().on_control(Box::new(move |device, line| state.events.send(device, line)));
    }

    // Flow B (intro.rs): a phone that says join is brought to this
    // computer's other computers, off the connection threads.
    {
        let state = Arc::clone(&state);
        std::thread::spawn(move || intro::supervise(state));
    }

    let control_path = crate::state::control_socket();
    let control = bind_control(&control_path)?;
    {
        let state = Arc::clone(&state);
        std::thread::spawn(move || control::serve(control, state));
    }

    eprintln!(
        "rime-remoted: listening on {port}, {} device(s) paired, identity {}",
        state
            .devices()
            .map(|s| s.list().iter().filter(|d| d.is_active()).count())
            .unwrap_or(0),
        // The PUBLIC key. It is what goes in a QR code and what a device
        // pins; printing it lets somebody check by eye that the code they
        // scanned belongs to this machine.
        state.identity.public_key()
    );

    let agentd = rime_agent_core::paths::control_socket();

    // The relay, when one is configured. Parsed here rather than at every
    // dial so a typo is one line in the journal at startup instead of a
    // failed connection every two seconds for the life of the machine — and
    // NOT a refusal to start, because a bad relay address must not cost the
    // owner the LAN path that does work.
    if let Some(url) = state.relay.clone() {
        match rime_remote_core::relay::Endpoint::parse(&url) {
            Ok(endpoint) => {
                let state = Arc::clone(&state);
                std::thread::spawn(move || relay::supervise(state, endpoint));
            }
            Err(e) => eprintln!(
                "rime-remoted: the configured relay is not usable, so this machine is \
                 reachable on this network only: {e}"
            ),
        }
    }

    // Push notifications (P1-058). Started unconditionally rather than only
    // when something is registered: a phone registers over a connection this
    // process has to be already serving, and a watcher started at that moment
    // would have an empty map and would therefore treat the machine's whole
    // current state as a first poll — which is right — but would also have
    // missed nothing, because the first poll raises nothing either way. What
    // running it from the start buys is that a transition happening two
    // seconds after a phone registers is an edge this watcher has both sides
    // of.
    //
    // It costs one `list` and one `requests` on a Unix socket every four
    // seconds, and a `worktrees` every minute, on a machine with an agent
    // runtime already running. `state.push_is_empty()` skips the delivery
    // half entirely when nobody is registered.
    {
        let state = Arc::clone(&state);
        let agentd = agentd.clone();
        std::thread::spawn(move || push::supervise(state, agentd));
    }

    // Presence between paired computers, and telling a removed one (§2.15).
    // With no paired computer the thread waits on a condition variable with
    // no timeout: no wakeups until a pairing, removal or forget.
    {
        let state = Arc::clone(&state);
        // Named, so `/proc/<pid>/task/*/comm` shows what it costs.
        std::thread::Builder::new()
            .name("rime-presence".into())
            .spawn(move || presence::supervise(state))
            .map_err(|e| format!("could not start the presence thread: {e}"))?;
    }

    // On the local network, for as long as this process runs. Deliberately
    // not a file in /etc/avahi/services, which would advertise the machine
    // whether or not the service was running; see discovery.rs.
    let _announced = (!args.iter().any(|a| a == "--no-announce")).then(|| discovery::announce(&state));

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        // A dead connection must not hold a thread forever, and a live PTY
        // must not be killed by a read deadline. So the handshake gets a
        // deadline and `serve::session` clears it once the connection has
        // authenticated — an unauthenticated peer never reaches the clear.
        //
        // The first version of this set the deadline here and then cleared it
        // as the first line of the spawned thread, which is the opposite of
        // what its own comment said: a LAN peer that connected and sent
        // nothing held a thread forever, on the only network listener in the
        // stack.
        prepare_accepted(&stream, handshake);
        let state = Arc::clone(&state);
        let agentd = agentd.clone();
        std::thread::spawn(move || serve::connection(stream, state, agentd));
    }
    Ok(())
}

/// The two options every accepted socket gets before its thread starts.
///
/// **`TCP_NODELAY`**, which this listener never set, so every socket it
/// accepted ran Nagle's algorithm: a small segment waits until the previous
/// one is acknowledged, and a phone delays its acknowledgements by up to
/// 40 ms. The traffic here is nothing BUT small segments — a keystroke echo,
/// a control reply, a ping — so each of them could sit out a delayed ACK, and
/// the handshake, which is a request and an answer, paid it on the way in.
/// The relay dial and the loopback splice already set it; this was the one
/// path that had been missed, and it is the LAN path, the fast one. Set on
/// the accepted descriptor, so every `try_clone` of it downstream has it too.
///
/// **The handshake deadline**, cleared by `serve::session` once the peer has
/// authenticated; see the comment at the call site.
///
/// A function of its own so a test can hold the socket afterwards and read
/// both options back.
fn prepare_accepted(stream: &std::net::TcpStream, handshake: std::time::Duration) {
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(handshake)).ok();
}

/// Refuse to run somewhere that would make every forwarded request look local.
fn guard_placement(allow_foreground: bool) -> Result<(), String> {
    match rime_agent_core::origin::is_a_user_service() {
        Ok(true) => Ok(()),
        Ok(false) if allow_foreground => {
            eprintln!(
                "rime-remoted: running outside a systemd user unit. Every connection this \
                 process opens to the agent runtime still declares itself \
                 claude-remote-control, but the placement check that would have caught a \
                 mistake there is off."
            );
            Ok(())
        }
        Ok(false) => Err(
            "this process is not a systemd user service, so the agent runtime would classify \
             every request it forwards as coming from a human at this machine. Start it with \
             `rime remote enable`, or pass --allow-foreground if you know why you want that"
                .to_string(),
        ),
        Err(why) if allow_foreground => {
            eprintln!("rime-remoted: cannot tell where this process is running ({why})");
            Ok(())
        }
        Err(why) => Err(format!(
            "cannot tell where this process is running ({why}), so it cannot be shown to be an \
             unattended service — and a proxy that might be classified as a human at this \
             machine must not start"
        )),
    }
}

/// Bind the local control socket, replacing a stale one.
fn bind_control(path: &PathBuf) -> Result<std::os::unix::net::UnixListener, String> {
    let dir = path.parent().ok_or("the control socket has no directory")?;
    rime_agent_core::paths::ensure_private_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    // A socket left behind by a killed process refuses `bind` with
    // AddrInUse, so it is removed first — but only after checking that
    // nothing is listening on it, or a second instance would take the first
    // one's socket away.
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return Err(format!(
            "another rime-remoted is already listening on {}",
            path.display()
        ));
    }
    let _ = std::fs::remove_file(path);
    std::os::unix::net::UnixListener::bind(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// What the QR code calls this machine.
///
/// The kernel hostname, which is what the owner already calls it everywhere
/// else. Falls back to a generic name rather than to an empty string, because
/// a device list with a blank entry in it is worse than one with `rime` in it.
fn machine_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "rime".to_string())
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

/// The service's own static secret, for the handshake.
///
/// A free function rather than a method so that `State` does not have to
/// expose it: nothing but the two handshake constructors ever needs it, and
/// both are in `serve`.
fn secret_of(state: &State) -> [u8; 32] {
    state.identity.secret_bytes_for_handshake()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_port_is_the_one_the_firewall_catalogue_names() {
        // The catalogue is the user-facing name for this port, and the two
        // drifting apart means `rime firewall allow rime-remote` opens a port
        // nothing is listening on.
        let catalogue = include_str!("../../../files/system/firewall/services");
        let line = catalogue
            .lines()
            .find(|l| l.starts_with("rime-remote "))
            .expect("rime-remote is not in the firewall catalogue");
        let port: u16 = line
            .split_whitespace()
            .nth(2)
            .and_then(|p| p.parse().ok())
            .unwrap_or_else(|| panic!("no port in {line:?}"));
        assert_eq!(port, DEFAULT_PORT);
        assert!(line.contains("tcp"), "{line}");
    }

    #[test]
    fn the_handshake_deadline_cannot_be_switched_off() {
        // It is a denial-of-service guard, so a flag that could raise it to an
        // hour or drop it to nothing would be a way to ask for the bug it
        // exists to prevent.
        let clamp = |ms: u64| std::time::Duration::from_millis(ms.clamp(1_000, 120_000));
        assert_eq!(clamp(0), std::time::Duration::from_secs(1));
        assert_eq!(clamp(u64::MAX), std::time::Duration::from_secs(120));
        assert_eq!(clamp(2_000), std::time::Duration::from_secs(2));
        assert!(serve::HANDSHAKE_TIMEOUT >= std::time::Duration::from_secs(1));
        assert!(serve::HANDSHAKE_TIMEOUT <= std::time::Duration::from_secs(120));
    }

    #[test]
    fn an_accepted_socket_has_nagle_off_and_a_handshake_deadline() {
        // Read back from the kernel on a real accepted socket rather than
        // assumed from the call: `set_nodelay(...).ok()` swallows a failure,
        // so the only evidence that it took is the option itself.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let _client = std::net::TcpStream::connect(addr).expect("connect");
        let (accepted, _) = listener.accept().expect("accept");
        assert!(!accepted.nodelay().expect("read TCP_NODELAY"), "the kernel default changed");
        prepare_accepted(&accepted, std::time::Duration::from_secs(7));
        assert!(accepted.nodelay().expect("read TCP_NODELAY"), "Nagle is still on");
        assert_eq!(
            accepted.read_timeout().expect("read SO_RCVTIMEO"),
            Some(std::time::Duration::from_secs(7))
        );
        // And a clone — which is what `serve` reads and writes through — is
        // the same descriptor's option, not a copy that could disagree.
        assert!(accepted.try_clone().expect("clone").nodelay().expect("read"));
    }

    #[test]
    fn the_usage_says_what_allow_foreground_gives_up() {
        // A flag that turns off a security check has to say so where the
        // person turning it on will read it.
        assert!(USAGE.contains("--allow-foreground"));
        assert!(USAGE.contains("login session"));
        assert!(USAGE.contains("claude-remote-control"));
    }

    #[test]
    fn a_machine_name_is_never_empty() {
        let name = machine_name();
        assert!(!name.trim().is_empty());
        assert!(!name.contains('\n'), "{name:?}");
    }

    #[test]
    fn flags_are_read_by_name_and_not_by_position() {
        let args: Vec<String> = ["--relay", "https://r.invalid", "--port", "9000"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag(&args, "--port").as_deref(), Some("9000"));
        assert_eq!(flag(&args, "--relay").as_deref(), Some("https://r.invalid"));
        assert_eq!(flag(&args, "--missing"), None);
        // A flag at the end with no value is absent rather than a panic.
        assert_eq!(flag(&["--port".to_string()], "--port"), None);
    }

    #[test]
    fn the_placement_guard_refuses_a_login_session_and_says_what_to_run() {
        // Runs against this process's real cgroup. The assertion is the rule
        // rather than a fixed answer: under `cargo test` in a terminal this
        // is a login session and must be refused, and in a container under a
        // user service it must be allowed.
        match (
            rime_agent_core::origin::is_a_user_service(),
            guard_placement(false),
        ) {
            (Ok(true), Ok(())) => {}
            (Ok(true), Err(e)) => panic!("a user service was refused: {e}"),
            (Ok(false), Err(e)) => {
                assert!(e.contains("rime remote enable"), "{e}");
                assert!(e.contains("a human at this machine"), "{e}");
            }
            (Ok(false), Ok(())) => panic!("a login session was allowed to start the proxy"),
            (Err(_), Err(e)) => assert!(e.contains("must not start"), "{e}"),
            (Err(why), Ok(())) => panic!("unclassifiable ({why}) and still started"),
        }
        // And the override is an override, wherever this runs.
        assert!(guard_placement(true).is_ok());
    }
}
