//! `rime mode`, `rime workload` and `rime perf` — the §11/§12/§13 surfaces.
//!
//! ## `rime mode` composes; it does not invent
//!
//! Setting a mode is a short sequence of the frozen `org.rimeos.Rimed1` calls a
//! user could type by hand: `Power.SetAutoSwitch`, `Power.SetTier`,
//! `GameMode.SetActive`. The active mode is still *derived* from what the
//! daemon reports, so it cannot go stale and `rime mode set` needs no root.
//!
//! One thing is kept (2026-10-10): the mode CHOSEN, through `Mode.Hold`, which
//! rimed writes to `/var/lib/rimed/mode` and applies again at every start.
//! Without it a reboot was Daily by construction, and Settings → Gaming's
//! "Gaming" lasted until the next boot or the next game to quit. `Hold` is
//! called before the levers move: holding Gaming makes `SetActive(false)`
//! refuse, so leaving it has to release the hold first (rimed/src/hold.rs).
//!
//! The polkit action behind the tier calls (`manage-power`) is `allow_active=yes`,
//! so an ordinary user on the seat authorises without a prompt. That matters
//! here beyond convenience: this is the phase whose earlier tests raised a burst
//! of authentication prompts on the developer's desktop, and nothing added here
//! may reintroduce one.
//!
//! ## The apply guard
//!
//! `RIME_MODE_NO_APPLY=1` makes `rime mode set` refuse before it connects to
//! anything. It exists so the shell suite can exercise the real shipped binary
//! without any chance of moving the tier of the machine running the tests — the
//! same reason `rime-display-apply` has `RIME_DISPLAY_NO_LIVE`. The check is
//! deliberately the FIRST thing `set` does, ahead of the bus connection, and the
//! suite proves that ordering rather than assuming it.
//!
//! ## Fixture roots
//!
//! `rime workload` and `rime perf` are pure readers, so they honour
//! `RIME_SYS_ROOT`, `RIME_PROC_ROOT` and `RIME_GAME_CGROUP`. That is what lets
//! the suite assert on real output for hardware this machine does not have —
//! an NVIDIA GPU, a machine without PSI — instead of only asserting whatever
//! the developer's laptop happens to be.

use rimed_core::gpu::{NvidiaSmi, RealNvidiaSmi};
use rimed_core::mode::{self, ModeId, Step, TierPolicy};
use rimed_core::perf::{self, PerfSnapshot};
use rimed_core::tier::Tier;
use rimed_core::workload::{self, Assessment, Roots, Signal};
use clap::{Args, Subcommand};

use crate::proxy::{connect, daemon_running, GameModeProxy, ModeProxy, PowerProxy};

/// The environment variable that makes `rime mode set` refuse to act.
pub const NO_APPLY_ENV: &str = "RIME_MODE_NO_APPLY";

#[derive(Subcommand)]
pub enum ModeCmd {
    /// List every mode with the policy it applies.
    List,
    /// Explain one mode: what it changes, what it only reports, and why.
    Show {
        #[arg(value_name = "MODE")]
        name: String,
    },
    /// Switch to a mode, or to the one the measured workload suggests.
    Set {
        #[arg(value_name = "MODE", required_unless_present = "auto")]
        name: Option<String>,
        /// Use the mode `rime workload` recommends instead of naming one.
        ///
        /// One-shot and explicit: Rime ships nothing that re-evaluates this on
        /// a timer. See `rime workload` for the reasoning behind the choice.
        #[arg(long, conflicts_with = "name")]
        auto: bool,
        /// Print the steps and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Which mode the machine is in, derived from what the daemon reports.
    Status,
}

#[derive(Args)]
pub struct WorkloadArgs {
    /// Emit machine-readable JSON instead of a report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct PerfArgs {
    /// Emit machine-readable JSON instead of an aligned table.
    #[arg(long)]
    pub json: bool,
}

/// The roots to read from, honouring the fixture overrides.
fn roots() -> Roots {
    let get = |k: &str| {
        std::env::var(k)
            .ok()
            .filter(|s| !s.trim().is_empty())
    };
    let mut r = Roots::live();
    if let Some(v) = get("RIME_SYS_ROOT") {
        r.game_cgroup = std::path::PathBuf::from(&v).join("fs/cgroup/rime-game");
        r.sys = v.into();
    }
    if let Some(v) = get("RIME_PROC_ROOT") {
        r.proc = v.into();
    }
    if let Some(v) = get("RIME_GAME_CGROUP") {
        r.game_cgroup = v.into();
    }
    r
}

/// The NVIDIA querier.
///
/// Suppressed entirely when a fixture root is set: a suite pointing at a canned
/// `/sys` must not have a real `nvidia-smi` on the host answer for it, which
/// would make the same assertion pass or fail depending on whose machine ran it.
fn querier() -> Box<dyn NvidiaSmi> {
    if std::env::var_os("RIME_SYS_ROOT").is_some() {
        return Box::new(rimed_core::gpu::MockNvidiaSmi::default());
    }
    Box::new(RealNvidiaSmi)
}

// ── rendering helpers ────────────────────────────────────────────────────────

/// Render a signal as `value` or `unavailable — reason`.
fn show<T>(s: &Signal<T>, fmt: impl Fn(&T) -> String) -> String {
    match s {
        Signal::Measured { value, .. } => fmt(value),
        Signal::Unavailable { reason, .. } => format!("unavailable — {reason}"),
    }
}

fn kv(key: &str, value: &str) {
    println!("{key:<14}: {value}");
}

fn tier_policy_str(p: TierPolicy) -> String {
    match p {
        TierPolicy::Auto => "auto (the profile's AC/battery defaults)".to_string(),
        TierPolicy::Pinned(t) => format!("{t} (pinned)"),
    }
}

// ── rime mode ────────────────────────────────────────────────────────────────

pub async fn main(cmd: ModeCmd) -> i32 {
    match cmd {
        ModeCmd::List => cmd_list(),
        ModeCmd::Show { name } => cmd_show(&name),
        ModeCmd::Status => cmd_status().await,
        ModeCmd::Set {
            name,
            auto,
            dry_run,
        } => cmd_set(name, auto, dry_run).await,
    }
}

fn cmd_list() -> i32 {
    let width = ModeId::ALL
        .iter()
        .map(|m| m.as_str().len())
        .max()
        .unwrap_or(0);
    println!("{:<width$}  TIER                         GAME  SUMMARY", "MODE", width = width);
    for id in ModeId::ALL {
        let m = id.spec();
        println!(
            "{:<width$}  {:<28} {:<5} {}",
            id.as_str(),
            tier_policy_str(m.tier),
            if m.game { "on" } else { "off" },
            m.summary,
            width = width
        );
    }
    println!();
    println!("A mode composes what `rime tier` and `rime game` already do.");
    println!("`rime mode show <mode>` explains one; `rime mode status` says where you are.");
    0
}

fn cmd_show(name: &str) -> i32 {
    let id: ModeId = match name.parse() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("rime: {e}");
            return 2;
        }
    };
    let m = id.spec();
    kv("mode", &format!("{} ({})", m.label, id));
    kv("summary", m.summary);
    kv("tier", &tier_policy_str(m.tier));
    kv("game mode", if m.game { "on" } else { "off" });
    kv(
        "intent",
        &match m.intent {
            Some(i) => format!("{i} — {}", i.describe()),
            None => "none (this mode expresses no single workload intent)".to_string(),
        },
    );
    println!();
    println!("why this policy:");
    for line in wrap(m.rationale, 72) {
        println!("  {line}");
    }

    // Reported, not applied — and labelled as such every single time, because a
    // report that reads like an action is how a user ends up believing a
    // service was moved when it was not.
    println!();
    println!("reported, NOT applied by `rime mode set`:");
    if m.services.is_empty() && m.sysext.is_empty() {
        println!("  (nothing)");
    }
    for s in m.services {
        println!("  service {} -> {} ({})", s.unit, s.want.as_str(), s.why);
    }
    for e in m.sysext {
        println!("  system extension {e}");
    }
    println!();
    println!(
        "  Service sets and system extensions are modelled so this page can state the\n  \
         full intent, but `rime mode set` does not move them. Merging a system\n  \
         extension is a heavyweight operation with its own rebuild service, and the\n  \
         Gaming image already masks irqbalance permanently."
    );
    0
}

/// Read the three observable facts a mode is matched against.
///
/// `pub(crate)` because `rime game profile` matches against exactly the same
/// three facts and must not grow a second reader — two readers is how the two
/// verbs come to disagree about which mode the machine is in.
pub(crate) async fn observe() -> Result<mode::ModeState, String> {
    let Some(conn) = connect().await else {
        return Err("cannot reach the system bus".to_string());
    };
    if !daemon_running(&conn).await {
        return Err("rimed is not running".to_string());
    }
    let power = PowerProxy::new(&conn)
        .await
        .map_err(|e| format!("cannot reach the Power interface: {e}"))?;
    let tier: Tier = power
        .tier()
        .await
        .map_err(|e| format!("cannot read the tier: {e}"))?
        .parse()
        .map_err(|e| format!("rimed reported a tier this CLI does not know: {e}"))?;
    let auto_switch = power
        .auto_switch()
        .await
        .map_err(|e| format!("cannot read auto-switch: {e}"))?;
    // A daemon built without game mode still answers the Power interface, so a
    // GameMode failure degrades to "not active" rather than failing the whole
    // read — `rime mode status` must keep working on such a machine.
    let game_active = match GameModeProxy::new(&conn).await {
        Ok(g) => g.active().await.unwrap_or(false),
        Err(_) => false,
    };
    Ok(mode::ModeState {
        tier,
        auto_switch,
        game_active,
    })
}

async fn cmd_status() -> i32 {
    let state = match observe().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rime: {e} — the active mode is derived from rimed's own state, so it cannot be shown.");
            eprintln!("      `rime mode list` and `rime mode show <mode>` work without the daemon.");
            return 1;
        }
    };
    kv("tier", state.tier.as_str());
    kv("auto-switch", if state.auto_switch { "on" } else { "off" });
    kv("game mode", if state.game_active { "on" } else { "off" });

    // What rimed puts back at its next start. Silent on an older rimed.
    if let Some(conn) = connect().await {
        if let Ok(p) = ModeProxy::new(&conn).await {
            if let Ok(h) = p.held().await {
                kv(
                    "kept",
                    if h.is_empty() { "daily (nothing held)" } else { h.as_str() },
                );
            }
        }
    }

    let m = mode::identify(&state);
    println!();
    if m.exact.is_empty() {
        kv("mode", "custom (no mode matches)");
        println!("closest is '{}', differing in:", m.closest);
        for d in &m.diffs {
            println!("  - {d}");
        }
        println!();
        println!("That is a supported state, not a fault: every mode is a starting point");
        println!("you are free to override. `rime mode set {}` would restore it.", m.closest);
    } else if m.exact.len() == 1 {
        kv("mode", m.exact[0].as_str());
    } else {
        kv(
            "mode",
            &m.exact
                .iter()
                .map(|i| i.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
        println!();
        println!("These modes are indistinguishable from observable state: they pin the same");
        println!("tier with the same game-mode setting, and differ only in the intent they");
        println!("declare and the service sets they report. Nothing readable off a running");
        println!("machine tells them apart, so all of them are named rather than one guessed.");
    }
    0
}

async fn cmd_set(name: Option<String>, auto: bool, dry_run: bool) -> i32 {
    // FIRST, ahead of the bus connection and every read: the guard that keeps a
    // test suite off the machine running it. The shell suite asserts this
    // ordering by pointing DBUS_SYSTEM_BUS_ADDRESS at nothing and checking
    // which message comes out.
    if !dry_run && guard_set() {
        eprintln!(
            "rime: refusing to apply — {NO_APPLY_ENV} is set.\n\
             \x20     This guard exists so a test suite can run the real binary without\n\
             \x20     moving the tier of the machine it runs on. Use --dry-run to see the\n\
             \x20     plan, or unset {NO_APPLY_ENV} to apply for real."
        );
        return 2;
    }

    let id = if auto {
        let a = workload::assess(&workload::gather(&roots(), querier().as_ref()));
        match a.recommended {
            Some(id) => {
                println!("rime: measured workload is '{}'.", a.workload);
                for e in &a.evidence {
                    println!("  - {e}");
                }
                println!("rime: that suggests mode '{id}'.");
                id
            }
            None => {
                eprintln!(
                    "rime: nothing measurable is distinctive right now, so --auto has no\n\
                     \x20     recommendation and will not guess. Reasoning:"
                );
                for e in &a.evidence {
                    eprintln!("  - {e}");
                }
                if !a.gaps.is_empty() {
                    eprintln!("rime: signals this machine could not report:");
                    for g in &a.gaps {
                        eprintln!("  - {g}");
                    }
                }
                return 1;
            }
        }
    } else {
        match name.as_deref().unwrap_or_default().parse::<ModeId>() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("rime: {e}");
                return 2;
            }
        }
    };

    let state = match observe().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rime: {e} — cannot read the current state, so no plan can be built.");
            return 1;
        }
    };

    // Keep the choice across restarts, BEFORE any lever moves (see the module
    // header). Also when the levers already match: "already in gaming" after a
    // boot of an older image still has to be remembered from now on.
    if !dry_run {
        if let Err(code) = hold(id).await {
            return code;
        }
    }

    let spec = id.spec();
    let steps = mode::plan(spec, &state);
    if steps.is_empty() {
        println!("rime: already in '{id}' — nothing to do.");
        return 0;
    }

    if dry_run {
        println!("rime: plan for '{id}' (dry run — nothing was changed):");
        for s in &steps {
            println!("  - {}", s.describe());
        }
        report_unapplied(spec);
        return 0;
    }

    let Some(conn) = connect().await else {
        eprintln!("rime: cannot reach the system bus.");
        return 1;
    };
    for step in &steps {
        if let Err(e) = apply(&conn, step).await {
            eprintln!("rime: {} failed: {e}", step.describe());
            eprintln!("rime: stopping here rather than applying a partial mode.");
            return 1;
        }
        println!("rime: {}", step.describe());
    }
    println!("rime: mode -> {id}");
    report_unapplied(spec);
    0
}

/// Tell rimed to keep `id` across restarts. An rimed without the `Mode`
/// interface (an image older than 2026-10-10) is not an error: the mode is set
/// as before and the user is told it will not survive a restart.
async fn hold(id: ModeId) -> Result<(), i32> {
    let Some(conn) = connect().await else {
        eprintln!("rime: cannot reach the system bus.");
        return Err(1);
    };
    let proxy = match ModeProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rime: cannot reach rimed's Mode interface: {e}");
            return Err(1);
        }
    };
    match proxy.hold(id.as_str()).await {
        Ok(()) => Ok(()),
        Err(zbus::Error::MethodError(name, _, _))
            if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod"
                || name.as_str() == "org.freedesktop.DBus.Error.UnknownInterface"
                || name.as_str() == "org.freedesktop.DBus.Error.UnknownObject" =>
        {
            eprintln!(
                "rime: this rimed cannot keep a mode across a restart (it predates\n\
                 \x20     `Mode.Hold`); '{id}' lasts until the next boot."
            );
            Ok(())
        }
        Err(zbus::Error::FDO(e)) if matches!(*e, zbus::fdo::Error::UnknownMethod(_)
            | zbus::fdo::Error::UnknownInterface(_)
            | zbus::fdo::Error::UnknownObject(_)) =>
        {
            eprintln!(
                "rime: this rimed cannot keep a mode across a restart (it predates\n\
                 \x20     `Mode.Hold`); '{id}' lasts until the next boot."
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("rime: keeping '{id}' across restarts failed: {e}");
            Err(1)
        }
    }
}

/// True when the apply guard is set to anything truthy.
///
/// `pub(crate)` for the same reason [`observe`] is: `rime game profile apply`
/// moves the same levers, so it must obey the same guard read the same way. A
/// second variable would split one invariant across two names and oblige a
/// third static check in `pr-validation.yml`.
pub(crate) fn guard_set() -> bool {
    matches!(
        std::env::var(NO_APPLY_ENV).ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Say what the mode declares but `set` did not do.
fn report_unapplied(m: &mode::Mode) {
    if m.services.is_empty() && m.sysext.is_empty() {
        return;
    }
    println!("rime: this mode also declares (reported, not applied):");
    for s in m.services {
        println!("  - service {} -> {}", s.unit, s.want.as_str());
    }
    for e in m.sysext {
        println!("  - system extension {e}");
    }
}

/// Perform one step over the frozen D-Bus surface.
async fn apply(conn: &zbus::Connection, step: &Step) -> Result<(), String> {
    match step {
        Step::AutoSwitch(v) => {
            let p = PowerProxy::new(conn).await.map_err(|e| e.to_string())?;
            p.set_auto_switch(*v).await.map_err(|e| e.to_string())
        }
        Step::SetTier(t) => {
            let p = PowerProxy::new(conn).await.map_err(|e| e.to_string())?;
            p.set_tier(t.as_str()).await.map_err(|e| e.to_string())
        }
        Step::GameMode(v) => {
            let g = GameModeProxy::new(conn).await.map_err(|e| e.to_string())?;
            g.set_active(*v).await.map_err(|e| e.to_string())
        }
    }
}

/// Wrap prose to a width, for the rationale block.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

// ── rime workload ────────────────────────────────────────────────────────────

pub fn workload_main(args: WorkloadArgs) -> i32 {
    let signals = workload::gather(&roots(), querier().as_ref());
    let a = workload::assess(&signals);
    if args.json {
        println!("{}", workload_json(&signals, &a));
        return 0;
    }

    kv("workload", a.workload.as_str());
    kv(
        "intent",
        &match a.intent {
            Some(i) => format!("{i} — {}", i.describe()),
            None => "none — not enough evidence to name one".to_string(),
        },
    );
    kv(
        "suggests",
        &match a.recommended {
            Some(m) => format!("mode {m}   (apply it with: rime mode set {m})"),
            None => "nothing; leave the current policy alone".to_string(),
        },
    );

    println!();
    println!("measured signals:");
    kv("  on AC", &show(&signals.on_ac, |v| v.to_string()));
    kv("  load (1m)", &show(&signals.load1, |v| format!("{v:.2}")));
    kv("  cpus", &show(&signals.cpus, |v| v.to_string()));
    kv(
        "  cpu pressure",
        &show(&signals.cpu_pressure, |v| format!("{v:.2}% (some avg10)")),
    );
    kv(
        "  io pressure",
        &show(&signals.io_pressure, |v| format!("{v:.2}% (some avg10)")),
    );
    kv(
        "  game session",
        &show(&signals.game_session, |v| format!("{v} process(es) confined")),
    );
    kv(
        "  vram",
        &show(&signals.vram, |v| {
            format!(
                "{:.1} / {:.1} GiB ({:.0}% used)",
                v.used_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                v.total_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                v.used_fraction() * 100.0
            )
        }),
    );
    if let Some(h) = signals.processes.value() {
        let mut named: Vec<String> = Vec::new();
        for (label, set) in [
            ("toolchain", &h.compiler),
            ("inference", &h.llm),
            ("render", &h.render),
            ("game", &h.game),
            ("browser", &h.browser),
        ] {
            if !set.is_empty() {
                named.push(format!(
                    "{label}: {}",
                    set.iter().cloned().collect::<Vec<_>>().join(" ")
                ));
            }
        }
        kv(
            "  processes",
            &if named.is_empty() {
                "nothing distinctive running".to_string()
            } else {
                named.join("; ")
            },
        );
    } else {
        kv("  processes", &show(&signals.processes, |_| String::new()));
    }

    println!();
    println!("how that was decided:");
    for e in &a.evidence {
        for (i, line) in wrap(e, 72).into_iter().enumerate() {
            println!("{}{line}", if i == 0 { "  - " } else { "    " });
        }
    }

    if !a.gaps.is_empty() {
        println!();
        println!("signals this machine cannot report:");
        for g in &a.gaps {
            for (i, line) in wrap(g, 72).into_iter().enumerate() {
                println!("{}{line}", if i == 0 { "  - " } else { "    " });
            }
        }
    }

    println!();
    println!("Nothing above has been applied. Rime ships no timer that re-evaluates");
    println!("this: acting on it is `rime mode set --auto`, which you run deliberately.");
    0
}

fn workload_json(s: &workload::Signals, a: &Assessment) -> String {
    use crate::json_string as js;
    let sig = |name: &str, rendered: Option<String>, source: &str, reason: Option<&str>| match rendered
    {
        Some(v) => format!(
            "{}:{{\"value\":{v},\"source\":{}}}",
            js(name),
            js(source)
        ),
        None => format!(
            "{}:{{\"value\":null,\"unavailable\":{},\"source\":{}}}",
            js(name),
            js(reason.unwrap_or("")),
            js(source)
        ),
    };
    let mut parts = vec![
        format!("\"workload\":{}", js(a.workload.as_str())),
        format!(
            "\"intent\":{}",
            a.intent.map(|i| js(i.as_str())).unwrap_or("null".into())
        ),
        format!(
            "\"recommended_mode\":{}",
            a.recommended
                .map(|m| js(m.as_str()))
                .unwrap_or("null".into())
        ),
        format!(
            "\"evidence\":[{}]",
            a.evidence.iter().map(|e| js(e)).collect::<Vec<_>>().join(",")
        ),
        format!(
            "\"gaps\":[{}]",
            a.gaps.iter().map(|g| js(g)).collect::<Vec<_>>().join(",")
        ),
    ];
    let mut sigs = Vec::new();
    sigs.push(sig(
        "on_ac",
        s.on_ac.value().map(|v| v.to_string()),
        s.on_ac.source(),
        s.on_ac.reason(),
    ));
    sigs.push(sig(
        "load1",
        s.load1.value().map(|v| format!("{v}")),
        s.load1.source(),
        s.load1.reason(),
    ));
    sigs.push(sig(
        "cpus",
        s.cpus.value().map(|v| v.to_string()),
        s.cpus.source(),
        s.cpus.reason(),
    ));
    sigs.push(sig(
        "cpu_pressure",
        s.cpu_pressure.value().map(|v| format!("{v}")),
        s.cpu_pressure.source(),
        s.cpu_pressure.reason(),
    ));
    sigs.push(sig(
        "game_session",
        s.game_session.value().map(|v| v.to_string()),
        s.game_session.source(),
        s.game_session.reason(),
    ));
    sigs.push(sig(
        "vram_used_bytes",
        s.vram.value().map(|v| v.used_bytes.to_string()),
        s.vram.source(),
        s.vram.reason(),
    ));
    sigs.push(sig(
        "vram_total_bytes",
        s.vram.value().map(|v| v.total_bytes.to_string()),
        s.vram.source(),
        s.vram.reason(),
    ));
    parts.push(format!("\"signals\":{{{}}}", sigs.join(",")));
    format!("{{{}}}", parts.join(","))
}

// ── rime perf ────────────────────────────────────────────────────────────────

pub fn perf_main(args: PerfArgs) -> i32 {
    let r = roots();
    let smi = querier();
    let snap = perf::snapshot(&r, smi.as_ref());
    let cpuset = perf::read_game_cpuset(&r);

    if args.json {
        println!("{}", perf_json(&snap));
        return 0;
    }

    println!("── CPU ──");
    kv(
        "clocks",
        &show(&snap.cpu.clocks, |c| {
            format!(
                "{} MHz mean, {}–{} MHz across {} policies",
                c.mean_mhz,
                c.min_mhz,
                c.max_mhz,
                c.per_policy.len()
            )
        }),
    );
    if let Some(c) = snap.cpu.clocks.value() {
        for (name, mhz) in &c.per_policy {
            kv(&format!("  {name}"), &format!("{mhz} MHz"));
        }
    }
    kv("governor", &show(&snap.cpu.governor, |v| v.clone()));
    kv("epp", &show(&snap.cpu.epp, |v| v.clone()));
    kv(
        "platform",
        &show(&snap.cpu.platform_profile, |v| v.clone()),
    );
    kv(
        "pressure",
        &show(&snap.cpu.pressure, |v| format!("{v:.2}% (some avg10)")),
    );

    println!();
    println!("── GPU ──");
    // Every card, before the headline figures, because on a hybrid laptop the
    // headline is about ONE of them and there is no honest way to say which
    // without naming it. `*` marks the card the rows below describe.
    if snap.gpu.devices.len() > 1 {
        for d in &snap.gpu.devices {
            let lead = if snap.gpu.clock_mhz.source() == d.clock_mhz.source() {
                "*"
            } else {
                " "
            };
            kv(
                &format!("{lead}{}", d.card),
                &format!(
                    "{} {}{} — clock {}, busy {}",
                    d.vendor,
                    d.driver.as_deref().unwrap_or("no driver bound"),
                    if d.boot_vga { ", boot display" } else { "" },
                    show(&d.clock_mhz, |v| format!("{v} MHz")),
                    show(&d.busy_percent, |v| format!("{v:.0}%")),
                ),
            );
        }
    }
    kv("clock", &show(&snap.gpu.clock_mhz, |v| format!("{v} MHz")));
    kv(
        "busy",
        &show(&snap.gpu.busy_percent, |v| format!("{v:.0}%")),
    );
    kv(
        "vram",
        &show(&snap.gpu.vram, |v| {
            format!(
                "{:.1} / {:.1} GiB ({:.0}% used, {:.1} GiB free)",
                v.used_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                v.total_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                v.used_fraction() * 100.0,
                v.free_bytes() as f64 / (1024.0 * 1024.0 * 1024.0),
            )
        }),
    );

    println!();
    println!("── Power and thermals ──");
    match snap.power_sources.value() {
        Some(sources) => {
            for r in sources {
                kv(&format!("  {}", r.name()), &format!("{:.2} W", r.watts));
            }
        }
        None => kv("power", &show(&snap.power_sources, |_| String::new())),
    }
    kv(
        "battery",
        &show(&snap.battery_watts, |v| format!("{v:.2} W")),
    );
    match snap.temps.value() {
        Some(temps) => {
            for t in temps {
                kv(&format!("  {}", t.name), &format!("{:.1} °C", t.celsius));
            }
        }
        None => kv("temps", &show(&snap.temps, |_| String::new())),
    }

    println!();
    println!("── Scheduler ──");
    kv(
        "sched_ext",
        &show(&snap.scheduler, |s| {
            let mut out = s.sched_ext.clone();
            if let Some(name) = &s.scheduler {
                out.push_str(&format!(" ({name})"));
            }
            if let Some(n) = s.rejected {
                out.push_str(&format!(", {n} rejected"));
            }
            out
        }),
    );
    kv(
        "game cpuset",
        &show(&cpuset, |c| {
            rimed_core::topology::format_cpu_list(c)
        }),
    );

    println!();
    println!("── Frame time ──");
    // Printed as a first-class row rather than omitted, so the reader can see
    // that Rime considered it and cannot measure it — an absent row would leave
    // them wondering whether it was measured and merely uninteresting.
    match snap.frame_time.reason() {
        Some(r) => {
            for (i, line) in wrap(r, 72).into_iter().enumerate() {
                println!("{}{line}", if i == 0 { "unavailable   : " } else { "                " });
            }
        }
        None => kv("frame time", "measured"),
    }
    0
}

fn perf_json(s: &PerfSnapshot) -> String {
    use crate::json_string as js;
    let num = |name: &str, v: Option<String>, source: &str, reason: Option<&str>| match v {
        Some(v) => format!("{}:{{\"value\":{v},\"source\":{}}}", js(name), js(source)),
        None => format!(
            "{}:{{\"value\":null,\"unavailable\":{},\"source\":{}}}",
            js(name),
            js(reason.unwrap_or("")),
            js(source)
        ),
    };
    let text = |name: &str, v: Option<String>, source: &str, reason: Option<&str>| match v {
        Some(v) => format!("{}:{{\"value\":{},\"source\":{}}}", js(name), js(&v), js(source)),
        None => format!(
            "{}:{{\"value\":null,\"unavailable\":{},\"source\":{}}}",
            js(name),
            js(reason.unwrap_or("")),
            js(source)
        ),
    };
    let mut parts = Vec::new();
    parts.push(num(
        "cpu_mhz_mean",
        s.cpu.clocks.value().map(|c| c.mean_mhz.to_string()),
        s.cpu.clocks.source(),
        s.cpu.clocks.reason(),
    ));
    parts.push(text(
        "governor",
        s.cpu.governor.value().cloned(),
        s.cpu.governor.source(),
        s.cpu.governor.reason(),
    ));
    parts.push(text(
        "epp",
        s.cpu.epp.value().cloned(),
        s.cpu.epp.source(),
        s.cpu.epp.reason(),
    ));
    parts.push(text(
        "platform_profile",
        s.cpu.platform_profile.value().cloned(),
        s.cpu.platform_profile.source(),
        s.cpu.platform_profile.reason(),
    ));
    parts.push(num(
        "gpu_mhz",
        s.gpu.clock_mhz.value().map(|v| v.to_string()),
        s.gpu.clock_mhz.source(),
        s.gpu.clock_mhz.reason(),
    ));
    parts.push(num(
        "gpu_busy_percent",
        s.gpu.busy_percent.value().map(|v| format!("{v}")),
        s.gpu.busy_percent.source(),
        s.gpu.busy_percent.reason(),
    ));
    parts.push(num(
        "vram_used_bytes",
        s.gpu.vram.value().map(|v| v.used_bytes.to_string()),
        s.gpu.vram.source(),
        s.gpu.vram.reason(),
    ));
    parts.push(num(
        "vram_total_bytes",
        s.gpu.vram.value().map(|v| v.total_bytes.to_string()),
        s.gpu.vram.source(),
        s.gpu.vram.reason(),
    ));
    // An object keyed by chip/label, never a bare "package_watts": the figure is
    // only meaningful alongside which sensor produced it.
    let power = match s.power_sources.value() {
        Some(v) => v
            .iter()
            .map(|r| format!("{}:{}", js(&r.name()), r.watts))
            .collect::<Vec<_>>()
            .join(","),
        None => String::new(),
    };
    parts.push(format!("\"power_watts\":{{{power}}}"));
    parts.push(num(
        "battery_watts",
        s.battery_watts.value().map(|v| format!("{v}")),
        s.battery_watts.source(),
        s.battery_watts.reason(),
    ));
    parts.push(text(
        "sched_ext",
        s.scheduler.value().map(|v| v.sched_ext.clone()),
        s.scheduler.source(),
        s.scheduler.reason(),
    ));
    parts.push(text(
        "scx_scheduler",
        s.scheduler.value().and_then(|v| v.scheduler.clone()),
        s.scheduler.source(),
        s.scheduler.reason(),
    ));
    // Always null, always with the reason attached. A consumer must not be able
    // to read a frame-time number out of this that Rime never measured.
    parts.push(num(
        "frame_time_ms",
        None,
        s.frame_time.source(),
        s.frame_time.reason(),
    ));
    let temps = match s.temps.value() {
        Some(t) => t
            .iter()
            .map(|x| format!("{}:{}", js(&x.name), x.celsius))
            .collect::<Vec<_>>()
            .join(","),
        None => String::new(),
    };
    parts.push(format!("\"temps_celsius\":{{{temps}}}"));
    format!("{{{}}}", parts.join(","))
}
