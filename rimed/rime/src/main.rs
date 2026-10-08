//! `rime` — the Rime OS control CLI. A thin client over the frozen
//! `org.rimeos.Rimed1` D-Bus API, with read-only local fallbacks (via
//! `rimed-core`) so `fingerprint`, `status`, `profile`, `doctor` and dry-run
//! tier planning work even when `rimed` is not running. Every D-Bus verb
//! degrades gracefully — a clear message, a non-zero exit, never a panic.

mod account;
mod agent;
mod ai;
mod backup;
mod blueprint;
mod channel;
mod boot;
mod browser;
mod cloudflare;
mod connector;
mod digest;
mod dispatch;
mod disposable;
mod firmware;
mod gaming;
mod gitshim;
mod host;
mod lid;
mod mcp;
mod migrate;
mod mode;
mod oauth_device;
mod ops;
mod permissions;
mod provenance;
mod proxy;
mod qualify;
mod recover;
mod remote;
mod schema;
mod request;
mod secret;
mod skill;
mod storage;
mod task;
mod touchpad;
mod trust;
mod user;
mod verify;
mod vm;

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rimed_core::tier::Tier;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::ops::LocalView;
use crate::proxy::{
    connect, daemon_running, BatteryProxy, FanProxy, GameModeProxy, MetricsProxy, PowerProxy,
    ProfileProxy,
};

#[derive(Parser)]
#[command(name = "rime", version, about = "Rime OS control CLI")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Full status: machine, profile, tier, battery.
    Status,
    /// Show the current tier, or switch to `name`.
    Tier { name: Option<String> },
    /// Show the resolved (layered) profile.
    Profile,
    /// Battery: status, charge thresholds, travel mode, calibration.
    Battery(BatteryArgs),
    /// Fans: report speeds, switch mode, restore firmware control.
    Fan {
        #[command(subcommand)]
        cmd: Option<FanCmd>,
    },
    /// Game mode: P-core pinning, IRQ steering, GPU clock locks, top tier.
    Game {
        #[command(subcommand)]
        cmd: GameCmd,
    },
    /// Named operating modes: daily, gaming, development, creator, ai, battery,
    /// couch, server.
    ///
    /// A mode is a named combination of things `rime tier` and `rime game`
    /// already do — it is not another image, and it adds no new hardware lever.
    /// The active mode is derived from what rimed reports rather than stored,
    /// so it cannot go stale and needs no root.
    Mode {
        #[command(subcommand)]
        cmd: Option<mode::ModeCmd>,
    },
    /// What the machine is measured to be doing, and what that suggests.
    ///
    /// Reports the workload, the signals behind it, and the signals this
    /// hardware cannot produce. Applies nothing: acting on it is an explicit
    /// `rime mode set --auto`, and Rime ships no timer that does it for you.
    Workload(mode::WorkloadArgs),
    /// Performance Lab: CPU/GPU clocks, power, temperatures, VRAM, scheduler.
    ///
    /// Read-only and root-free. Frame time is reported as unavailable with the
    /// reason, because no generic source for it exists and Rime will not
    /// substitute a number it did not measure.
    Perf(mode::PerfArgs),
    /// Whether this machine can boot straight into a controller-first Gaming
    /// Mode, and whether it is set to.
    ///
    /// Read-only. `rime game` is the hardware lever (cpuset, IRQ steering, GPU
    /// clocks); this is the §12 experience around it — the greeter's Gaming
    /// Mode entry, the gamescope session, the Desktop<->Gaming switch and any
    /// attached controllers. Exits non-zero when Gaming Mode would not start,
    /// so it is usable as a check.
    Gaming(gaming::GamingArgs),
    /// Keep working with the lid shut, and say what that cost (P1-063).
    ///
    /// The lever is a logind `handle-lid-switch` block inhibitor held only
    /// while there is live work, never an edit to `HandleLidSwitch=`: a machine
    /// with nothing running must suspend in a bag exactly as it does today.
    /// Thermal and battery guards suspend anyway and name themselves, and
    /// `rime lid report` says what the last closed period actually did — how
    /// long, what ran, whether the VPN held, what it cost in battery.
    Lid {
        #[command(subcommand)]
        cmd: Option<lid::LidCmd>,
    },
    /// What applications may touch, who enforces it, and what a revocation
    /// would actually do (P1-061).
    ///
    /// Read-only unless you ask for `revoke`. Every row carries the enforcer
    /// beside the answer, because they are not the same statement: a Flatpak
    /// whose manifest says `devices=all` opens /dev/video0 directly and is no
    /// more restrained than a native binary, and there is no microphone portal
    /// at all in any version of xdg-desktop-portal. `revoke` exits non-zero
    /// with the reason where nothing can be revoked, rather than reporting a
    /// success it did not achieve.
    Permissions {
        #[command(subcommand)]
        cmd: Option<permissions::PermCmd>,
    },
    /// What verified this boot, and what the boot counter believes (§22).
    ///
    /// Read-only. Rime is moving every machine to systemd-boot, and `rime
    /// update` migrates one in place when it can be done safely; a machine
    /// still on GRUB is reported as the normal state rather than as a fault.
    /// Boot counting, signed UKIs and TPM-bound unlock all live on the
    /// systemd-boot path, and this is the command that says which of them is
    /// actually in effect on this machine.
    Boot {
        #[command(subcommand)]
        cmd: boot::BootCmd,
    },
    /// Which update channel this machine follows, and how the last update
    /// went (§26).
    ///
    /// stable, candidate, beta and edge are four tags on one image. `status`
    /// says which one this machine is on — including the answer for a machine
    /// installed before channels existed, which is edge under an older name.
    /// `set` moves between them; moving toward stable usually deploys an older
    /// image, so that direction pins the current deployment first.
    Channel {
        #[command(subcommand)]
        cmd: channel::ChannelCmd,
    },
    /// What this class of machine is known to do, and who established it (§33).
    ///
    /// A local database, kept only with explicit consent and sent nowhere.
    /// Every check has three answers rather than two: a row nobody has tried
    /// reads as not known, with the sentence saying who can settle it, because
    /// "nobody has suspended this machine" is not "suspend is broken".
    Qualify {
        #[command(subcommand)]
        cmd: qualify::QualifyCmd,
    },
    /// The disks in this machine, their health, and what nobody could ask
    /// them (§48).
    ///
    /// Wear, temperature, TRIM, encryption, mount state and free space, with
    /// every row carrying either a measurement or the reason there is none.
    /// Reading needs no root except for the SMART log, which reports as
    /// unavailable with the remedy rather than disappearing.
    Storage {
        #[command(subcommand)]
        cmd: storage::StorageCmd,
    },
    /// Firmware: what this machine carries and what has an update waiting
    /// (§P2-015).
    ///
    /// Reads fwupd's own JSON and never its exit status — measured, that
    /// status means "nothing to do" when it is non-zero and accompanies an
    /// explicit error document when it is zero. Secure Boot key and
    /// revocation stores are listed apart from hardware, because most of what
    /// fwupd calls updatable is one of those rather than a component.
    Firmware {
        #[command(subcommand)]
        cmd: firmware::FirmwareCmd,
    },
    /// Persistent state: which schema each store is on, and what a rollback
    /// would do to it (§25).
    ///
    /// `bootc rollback` puts /usr back and leaves /etc, /var and your home
    /// exactly as the newer build left them. `status` says which files that
    /// applies to and whether the older Rime can still read each one;
    /// `migrate` runs the machine-written ones forward, keeping a copy of what
    /// each was. Neither needs root, and `migrate` is a dry run without
    /// --commit.
    Schema {
        #[command(subcommand)]
        cmd: schema::SchemaCmd,
    },
    /// Whether the image this machine runs is the one Rime published (§27).
    ///
    /// Reports what was checked when the booted image was pulled, what the
    /// signature policy will check on the next update, and — with `--verify` —
    /// whether the registry holds a cosign signature and an SBOM attestation
    /// for the digest running right now. The offline half reads files only, so
    /// it needs no root and no network; a registry that cannot be reached is
    /// reported as unavailable with the reason, never as unsigned.
    Trust(trust::TrustArgs),
    /// Local model inference as an OS service (§14).
    ///
    /// One endpoint every application and agent client can use — a Unix socket
    /// in your `$XDG_RUNTIME_DIR` speaking the runtime's own
    /// OpenAI-compatible HTTP API — with Rime owning the model store, the
    /// backend choice (CUDA, ROCm, Vulkan or CPU), how much fits in VRAM, and
    /// when an idle model is unloaded.
    ///
    /// The service is **per-user**, like `rime agent`, and for a stronger
    /// reason: it turns your prompts into generated text, so it must not be a
    /// privileged daemon shared between accounts. The weights are shared
    /// instead — one root-owned, read-only copy under /var that no session can
    /// alter, including its own.
    ///
    /// What it deliberately does NOT do: listen on a TCP port. A TCP
    /// connection carries no peer credential, so a listener on 127.0.0.1 is
    /// reachable by every account on the machine and by every sandboxed
    /// application that holds the network permission. `--listen` exists only to
    /// say so. It also ships no inference runtime — llama.cpp with CUDA is
    /// gigabytes, and `Containerfile.core` is the tier a rebuild makes the
    /// whole fleet download — so `rime ai status` names the `rime install` or
    /// `rime env` command that provides one.
    ///
    /// Needs the per-user service: `systemctl --user enable --now rime-aid`.
    Ai {
        #[command(subcommand)]
        cmd: ai::AiCmd,
    },
    /// Trusted Rime devices, and what each one can do (§20).
    ///
    /// A device is named by an ssh destination — normally an alias already in
    /// `~/.ssh/config`, which is why there is no address, key or port to repeat
    /// here. Rime generates no key and holds no passphrase: authentication,
    /// host identity and transport are whatever `ssh <destination>` already
    /// does, including a ProxyCommand or a Match exec that picks a route per
    /// network.
    ///
    /// Capabilities are *probed*, never assumed. A Rime peer answers
    /// `rime host describe --json` with the same struct this side parses; a
    /// host that is not Rime gets a portable shell probe so `list` still says
    /// something true about it.
    Host(host::HostArgs),
    /// Build this project, here or on a trusted device (§20).
    ///
    /// Without `--on` it builds locally, running the same command it would
    /// dispatch — so a local run and a remote one cannot drift apart about
    /// what "the build" is. The command is detected from the project's marker
    /// files and *printed*, because a detector silently choosing between five
    /// possibilities is one nobody can correct; give it explicitly after `--`
    /// to override.
    ///
    /// The remote directory is assumed to be the same absolute path and then
    /// **verified** — it must exist and be the same repository, compared by
    /// `origin` URL — because the failure mode of a wrong guess is a build
    /// that succeeds against the wrong source.
    Build(dispatch::BuildArgs),
    /// Send files or the clipboard to a trusted device (§20).
    ///
    /// Files land under their own name, not the sender's directory layout, and
    /// an existing file is NOT overwritten unless `--force` says so: a send
    /// that replaced something on another machine is not recoverable from this
    /// end.
    Send(dispatch::SendArgs),
    /// Open a URL or a path on a trusted device's screen (§20).
    ///
    /// Needs a graphical session there, and checks for one: an ssh command has
    /// no session bus, and a machine sitting at its greeter would otherwise
    /// report success and open something nobody can see.
    Open(dispatch::OpenArgs),
    /// Print the hardware fingerprint and layered profile selection.
    Fingerprint,
    /// Pin the current deployment (ostree admin pin 0). Requires root.
    Pin,
    /// Roll back to the previous deployment (bootc rollback). Requires root.
    Rollback,
    /// Update the OS image (bootc upgrade) and firmware (fwupdmgr). Requires root.
    Update(UpdateArgs),
    /// Drive Rime Shell: open the launcher, dashboard, settings window, lock
    /// screen and the quick toggles.
    ///
    /// A thin wrapper over the shell's Quickshell IPC. It exists so compositor
    /// configs and scripts have one stable, readable command instead of
    /// spelling out `qs -p /usr/share/rime-shell ipc call <target> <fn>`, and so
    /// the shell's install path is not hardcoded in every keybind.
    Shell {
        #[command(subcommand)]
        cmd: ShellCmd,
    },
    /// Read the telemetry snapshot: tier, AC state, package power, battery
    /// charge and thermal zones.
    ///
    /// Values come from rimed's `org.rimeos.Rimed1.Metrics.Snapshot`, the same
    /// source as the Prometheus endpoint on 127.0.0.1:9723. Read-only, so it
    /// needs no root.
    Metrics(MetricsArgs),
    /// Diagnose the power stack.
    ///
    /// `--json` is §19's "expose `rime doctor` results graphically" from the OS
    /// side: the same checks, in the shape a UI renders. Not a second set of
    /// checks — the list is built once and rendered either way, because two
    /// diagnostic implementations disagree and the one the user reads would be
    /// the one wired to nothing.
    Doctor {
        /// Emit machine-readable JSON instead of PASS/WARN lines.
        #[arg(long)]
        json: bool,
    },
    /// Show the booted image and its changelog labels.
    Changelog,
    /// Install packages from the enabled repositories, Flathub, a capsule, a
    /// local .rpm file, or an AppImage. Requires root, except `--source capsule`.
    ///
    /// Each argument is a package name from Fedora/RPM Fusion/an enabled COPR, a
    /// reverse-DNS Flatpak id (org.gimp.GIMP), or a path to an .rpm file. A local
    /// file is copied into /var/lib/rime/pkg/local so later rebuilds no longer
    /// need the original; its dependencies still come from the repositories.
    ///
    /// Packages go into a systemd system extension, NOT an rpm-ostree layer, so
    /// the OS keeps updating normally and `rime rollback` still works.
    ///
    /// An AppImage is different and the difference is worth knowing before you
    /// install one: it is unpacked once into /usr/local, never executed as a
    /// file, never part of the extension — and PINNED. `rime update` does not
    /// move it and it cannot update itself; a newer version means running this
    /// command again with the newer file. Every AppImage needs
    /// --allow-unsigned, because none of them carries a signature Rime can
    /// check. See docs/packages.md.
    Install {
        #[arg(required = true, value_name = "PACKAGE|FILE.rpm|FILE.AppImage")]
        packages: Vec<String>,
        /// Skip weak dependencies (smaller install, fewer optional features).
        #[arg(long)]
        no_weak_deps: bool,
        /// Also consider a repository that is disabled by default.
        #[arg(long, value_name = "REPO")]
        enable_repo: Vec<String>,
        /// Install a local .rpm file that no trusted key covers, or an
        /// AppImage (no AppImage is ever verifiable, so all of them need it).
        /// Applies only to the files named on this command line, never to
        /// repository packages, and the decision is recorded per file so
        /// `rime pkg list` and `rime pkg verify` keep reporting it.
        #[arg(long)]
        allow_unsigned: bool,
        /// Pick the source yourself instead of letting Rime rank them:
        /// rpm (the system extension), flatpak, or capsule.
        ///
        /// Applies to bare names only. A path is always an RPM and an
        /// application id is always a Flatpak, so naming a source that
        /// contradicts one of those is refused rather than quietly resolved.
        /// `rime resolve <name>` shows what would happen without it.
        #[arg(long, value_name = "SOURCE")]
        source: Option<String>,
        /// Which capsule `--source capsule` installs into. Defaults to your
        /// first one.
        #[arg(long, value_name = "CAPSULE")]
        env: Option<String>,
    },
    /// Show which source Rime would install a name from, and why.
    ///
    /// Prints every candidate across the repositories, Flathub and your
    /// capsules, what vouches for each one, the choice Rime would make, and
    /// the exact command for every alternative. Read-only, so it needs no
    /// root — "what would this do" should never cost a password.
    Resolve {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Remove packages installed with `rime install`. Requires root.
    ///
    /// A package installed from a local .rpm is removed by its package name;
    /// an AppImage by the command name it installed, or by the path to the
    /// very file it came from — that one is matched by checksum, so the same
    /// download in a different directory still resolves.
    Remove {
        #[arg(required = true, value_name = "PACKAGE|FILE.AppImage")]
        packages: Vec<String>,
    },
    /// Search every package source: the enabled repositories and Flathub.
    ///
    /// `rime resolve <name>` then says which of them Rime would actually use
    /// for a given name, and why.
    Search {
        #[arg(required = true, value_name = "TERM")]
        terms: Vec<String>,
    },
    /// Manage additional package repositories.
    Repo {
        #[command(subcommand)]
        cmd: RepoCmd,
    },
    /// Manage installed packages: list, status, rebuild, rollback, adopt.
    Pkg {
        #[command(subcommand)]
        cmd: PkgCmd,
    },
    /// Rime Capsules: development environments that leave the host alone.
    ///
    /// /usr is read-only and packages come from a system extension, which is
    /// the right shape for an operating system and the wrong one for
    /// ecosystems that expect a mutable userspace — `pip install --user`,
    /// `npm -g`, an SDK that wants /opt, a package manager that wants
    /// /etc/apt. A capsule gives each of those its own rootless container
    /// that still sees your home, your terminal and your devices.
    ///
    /// Unprivileged: capsules belong to you, not to the machine, so none of
    /// these verbs needs (or accepts) root.
    Env {
        #[command(subcommand)]
        cmd: EnvCmd,
    },
    /// Run and supervise coding agents on managed terminals.
    ///
    /// Rime owns the PTY, the sandbox and the project state; the agent itself
    /// is the ordinary upstream binary (`claude`, `opencode`, `codex`, …) in an
    /// ordinary terminal. Sessions outlive the window they were started from,
    /// so a closed terminal never kills a running task.
    ///
    /// Needs the per-user runtime: `systemctl --user enable --now rime-agentd`.
    Agent {
        #[command(subcommand)]
        cmd: agent::AgentCmd,
    },
    /// Rime Shell plugins: what is installed, and whether the shell will load it.
    ///
    /// The shell's plugin platform (§16) owns every rule about a manifest — the
    /// permission vocabulary, which permissions apiVersion 1 will actually
    /// grant, the import allowlist, the forbidden constructs. This command asks
    /// that validator rather than reimplementing it, so a verdict here is the
    /// verdict the shell will reach. If the shell is not installed, it refuses
    /// instead of guessing.
    ///
    /// Unprivileged: plugins live in your own `~/.config/rime-shell/plugins`.
    Plugin {
        #[command(subcommand)]
        cmd: PluginCmd,
    },
    /// Agent skills: where each came from, what it hashes to, and whether it
    /// ships a program.
    ///
    /// `rime agent profile doctor` already counts skills and names the ones
    /// with no `SKILL.md`. This is the inventory rather than the health check:
    /// per skill, its origin, a digest of the files on disk, and whether it is
    /// executable or reference-only — none of which anything recorded before.
    ///
    /// Unprivileged, and read-only: it never writes to a skill.
    Skill {
        #[command(subcommand)]
        cmd: SkillCmd,
    },
    /// Where an agent plugin came from, and whether what is on disk is still
    /// what arrived.
    ///
    /// **Not `rime plugin`**, which is rime-shell's QML plugin platform. This
    /// is the agent's plugins — Claude Code's, under `~/.claude/plugins`,
    /// installed from marketplaces.
    ///
    /// The registry already records a marketplace, a version and an install
    /// path, and nothing has ever checked any of it against the bytes on disk:
    /// there is no hash in it at all. This re-hashes each installed tree every
    /// run and compares it with a recorded baseline, so a plugin edited after
    /// it was installed is a finding rather than a green tick.
    ///
    /// Unprivileged. `show` is read-only; `record` writes only Rime's own
    /// baseline store.
    Provenance {
        #[command(subcommand)]
        cmd: ProvenanceCmd,
    },
    /// The incoming firewall: what is dropped, and the exceptions you opened.
    ///
    /// Rime drops inbound traffic by default. Reading needs no privilege;
    /// changing an exception needs root.
    Firewall {
        #[command(subcommand)]
        cmd: FirewallCmd,
    },
    /// Printers, scanners, shares, cards, links, radios and docks: what is
    /// there, what is not, and what could not be looked at.
    ///
    /// The third answer is the point. A failed stat is falsy and an empty list
    /// is falsy, so one line of code turns "permission denied" and "there are
    /// none" into the same report — which is how `rime recover status` came to
    /// tell users with packages installed that they had none. Nothing here
    /// folds an unreadable path, an absent daemon or a refused D-Bus call into
    /// an absence.
    ///
    /// It also names the firewall where a service this machine offers is what
    /// the policy is dropping, because "the printer does not work" is what the
    /// user sees and "631 is closed" is what is true.
    ///
    /// Unprivileged, and read-only: it pairs nothing, scans for nothing,
    /// authorises no dock and changes no connection. Every one of those is a
    /// polkit action, and a status command that raises an authentication dialog
    /// is a status command nobody runs twice.
    Devices {
        /// Which area to report. Everything, if you do not say.
        #[arg(value_enum)]
        area: Option<DeviceArea>,
    },
    /// Rime Remote: pair a phone with this machine, and take it away again.
    ///
    /// The phone talks to `rime-remoted`, a per-user unprivileged service that
    /// is a client of the agent runtime rather than part of it. Everything it
    /// forwards is recorded as `claude-remote-control`, so a remote request can
    /// edit a project, run tests and push — and cannot approve a root
    /// operation or start a break-glass session, whichever device asks.
    ///
    /// Reading and revoking need no privilege. Pairing needs you: an agent
    /// cannot pair a device on your behalf.
    Remote {
        #[command(subcommand)]
        cmd: remote::RemoteCmd,
    },
    /// Projects, agent worktrees and checkpoints.
    Project {
        #[command(subcommand)]
        cmd: agent::ProjectCmd,
    },
    /// What you are working on: the binder that can be put down and picked back
    /// up (§21).
    ///
    /// A task NAMES a project, a capsule, an agent worktree and the agents you
    /// run, and `rime task resume` checks that every one of them is still there
    /// before telling you how to continue — a task whose capsule was deleted or
    /// whose worktree was removed is refused by name rather than half-resumed.
    ///
    /// It creates none of those things and it grants nothing. There is
    /// deliberately no window list (windows come from
    /// `rime project layout save`) and no permission of any kind: §4's brokers
    /// own those, and a permission in a hand-editable file would be a grant
    /// nobody reviewed.
    ///
    /// Unprivileged: a task is yours, kept in your own `~/.config/rime` and
    /// `~/.local/state/rime`, so none of these verbs needs (or accepts) root.
    Task(task::TaskArgs),
    /// Structured privilege requests: how a sandboxed agent asks for a system
    /// change, and how you decide.
    ///
    /// An agent has no sudo, no root shell, and a sandbox that cannot reach the
    /// system bus. It files a request naming one of a closed set of operations
    /// and a reason; you review it and either refuse or approve, and approving
    /// runs the operation with YOUR privilege. There is deliberately no verb
    /// for an arbitrary command.
    Request {
        #[command(subcommand)]
        cmd: request::RequestCmd,
    },
    /// Connect a Cloudflare account, and see what this project binds.
    ///
    /// `rime cf connect` runs OAuth by device code: it prints a URL and a short
    /// code for you to enter on any device that has a browser, and launches
    /// nothing here. `--token` pastes a scoped token instead, from stdin.
    /// Either way the credential goes into `rime-secretd`'s root-owned store —
    /// not a dotfile, which an agent could read.
    #[command(visible_alias = "cf")]
    Cloudflare {
        #[command(subcommand)]
        cmd: cloudflare::CloudflareCmd,
    },

    /// Encrypted backups: local, NAS or an R2 bucket through the broker.
    ///
    /// A snapshot is sealed to a public key, so taking one needs no privilege
    /// and no secret. Only the private half opens one, and it is root-owned —
    /// which is what stops anything running as you from reading what your
    /// backups hold.
    Backup {
        #[command(subcommand)]
        cmd: backup::BackupCmd,
    },

    /// Online accounts: Nextcloud, Google, Microsoft, WebDAV, S3/R2.
    ///
    /// An account is a credential in the same root-owned store `rime secret`
    /// uses, under a reserved name, so there is no second place a cloud
    /// credential can be. What this adds is the provider table: it knows the
    /// endpoint, how the credential is presented, and which operation a scope
    /// like `files.read` grants.
    Account {
        #[command(subcommand)]
        cmd: account::AccountCmd,
    },

    /// The secret service: let an agent USE a credential without holding it.
    ///
    /// `rime-secretd` keeps every credential in a root-owned store, performs
    /// the operation itself, and returns the result. It has no verb that
    /// returns a credential. A git credential helper cannot achieve that — git
    /// runs inside the sandbox, so whatever the helper prints is readable by
    /// the agent.
    Secret {
        #[command(subcommand)]
        cmd: secret::SecretCmd,
    },

    /// The `git` a managed session finds first on its PATH. Not typed by hand.
    ///
    /// Runs `push`, `fetch` and `ls-remote` through the broker when the remote
    /// has a stored credential, and execs the real git for everything else. It
    /// holds no credential and enforces nothing — `/usr/bin/git` is still
    /// there, and reaches the same remotes with no credential at all.
    #[command(hide = true)]
    GitShim {
        /// Everything the session typed after `git`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// MCP servers Rime brokers, so a bearer token is not in the agent's config.
    ///
    /// `rime mcp bridge <service>` is an MCP server on stdin and stdout that
    /// carries each message through `rime-secretd`, which holds the credential
    /// and attaches it. Meant to be spawned by an agent rather than typed;
    /// `rime secret migrate` is what puts it in an agent's configuration.
    Mcp {
        #[command(subcommand)]
        cmd: mcp::McpCmd,
    },

    /// The declarative Rime Blueprint: what this machine should be.
    ///
    /// One TOML file — `~/.config/rime/blueprint.toml` — describes the desktop,
    /// applications, development languages, agent defaults and gaming. `rime
    /// blueprint diff` shows how the machine differs from it and `rime apply`
    /// converges it.
    ///
    /// The blueprint is yours: nothing in Rime rewrites it. `rime apply` writes
    /// its own record somewhere else (`rime blueprint show` prints where), so
    /// generated state and the file you edit never share a path.
    Blueprint {
        #[command(subcommand)]
        cmd: BlueprintCmd,
    },

    /// Converge this machine toward its blueprint.
    ///
    /// Idempotent: running it twice does nothing the second time, because the
    /// plan is recomputed from a fresh measurement of the machine every time
    /// rather than from a record of what was done last.
    ///
    /// It converges only the privilege domain it is already running in and
    /// reports the other. `rime apply` sets your desktop colour scheme and
    /// agent defaults; `sudo rime apply` selects the session and installs
    /// applications. Nothing here ever calls sudo itself, so `apply` cannot
    /// raise an authentication prompt — and a root run can never write
    /// root-owned files into your ~/.config.
    ///
    /// Applications are added, never removed. A package missing from the
    /// blueprint is left alone: reading a deleted line as "uninstall it" turns
    /// an edit into data loss.
    ///
    /// Setting RIME_BLUEPRINT_NO_APPLY to any non-empty value makes this refuse
    /// to change anything. --dry-run keeps working with it set.
    Apply(ApplyArgs),

    /// Carry settings, applications and projects to another Rime machine.
    ///
    /// `rime sync export` writes one file; `rime sync import` reads it on the
    /// other machine. The bundle carries the blueprint, which projects exist
    /// and where they came from, and nothing else — no credentials of any
    /// kind, because this is a file people put in a git repository.
    ///
    /// `import` never converges anything. It writes the blueprint and records
    /// the projects, and leaves `rime blueprint diff` and `rime apply` as
    /// separate decisions.
    Sync {
        #[command(subcommand)]
        cmd: SyncCmd,
    },

    /// Recovery, repair and rollback, in one surface (§19).
    ///
    /// `status` reports every component §19 names — the booted deployment, the
    /// rollback target, Secure Boot, the filesystem, the GPU driver, Rime
    /// Shell, the network and the package extension — with the action that
    /// addresses each one. It spawns no subprocess and contacts nothing, so it
    /// is safe for Rime Settings to poll and can never raise an authentication
    /// prompt.
    ///
    /// `repair` runs only steps that are idempotent and remove no data.
    /// `reset` is the scoped factory reset, and it is a dry run unless it is
    /// given both --commit and a token derived from the plan it printed.
    ///
    /// Rolling back is `rime rollback`, which already exists; this surface
    /// makes it visible rather than adding a second name for it.
    Recover {
        #[command(subcommand)]
        cmd: recover::RecoverCmd,
    },

    /// Disposable environments: a capsule that is deleted when you close it (§19).
    ///
    /// A mode of `rime env`, not a second mechanism. Every disposable
    /// environment is an ordinary Rime capsule created through the same engine
    /// and visible to `rime env list` — it just gets its own throwaway home,
    /// an explicit copy-in/copy-out boundary, and a teardown that removes the
    /// container and the directory together.
    ///
    /// It is a disposable ENVIRONMENT, not a security boundary: distrobox
    /// mounts the host filesystem at /run/host inside every capsule, and
    /// `rime disposable plan` prints that in full before anything starts. For
    /// confinement — a default-deny mount namespace with $HOME masked — the
    /// mechanism is `rime agent`'s sandbox.
    ///
    /// Unprivileged, like `rime env`, and for the same reason.
    Disposable {
        #[command(subcommand)]
        cmd: disposable::DisposableCmd,
    },
    /// Accounts on a shared machine: standard vs administrator, and guests
    /// (P2-016).
    ///
    /// Rime enforced the standard/administrator distinction long before it
    /// could make one. polkit's `auth_admin` guards both agent actions with
    /// `allow_any=no` and `allow_inactive=no`, and that is asserted at build
    /// time — but `installer/rime-install` puts the one account it creates in
    /// `wheel` unconditionally and its GUI offers no choice, so every account
    /// Rime has ever made is an administrator and a standard one could not be
    /// reached from any Rime surface. This is that surface.
    ///
    /// `rime user list` needs no root. Everything that changes an account
    /// does, and says so before it does anything.
    User {
        #[command(subcommand)]
        cmd: user::UserCmd,
    },
    /// Virtual machines: a full guest with its own kernel (P2-008).
    ///
    /// Not a second `rime env`. A capsule shares this kernel and this home; a
    /// VM shares neither, which is what makes it the right tool for booting
    /// another operating system, testing the installer, or running something
    /// that may take its kernel down with it.
    ///
    /// Rootless and session-scoped: every domain lives at `qemu:///session`,
    /// so there is no system daemon, no polkit prompt, and no `virbr0` left
    /// behind on the host. Headless: the console is serial, there is no
    /// viewer.
    ///
    /// The stack it drives — qemu, libvirt, OVMF, swtpm, virtiofsd — is
    /// userspace and is NOT in the image; the KVM kernel modules are, because
    /// a kernel module cannot be added at runtime under Secure Boot and
    /// userspace can. `rime vm doctor` prints the one command that installs
    /// the rest.
    Vm {
        #[command(subcommand)]
        cmd: vm::VmCmd,
    },
    /// P2-012's browser automation capsule: a browser that automates a site
    /// without going near the one you use.
    ///
    /// Its own profile, its own cookie jar, its own download directory, no
    /// route onto the network except the destinations you name, and nothing
    /// left behind. It is not a new sandbox: a capsule is a confined,
    /// allowlisted `rime agent` session, so the masked home and the egress
    /// proxy have one implementation rather than two.
    ///
    /// Headless, and structurally so — the capsule's /run is a tmpfs, so
    /// there is no compositor socket for a window to appear on.
    Browser {
        #[command(subcommand)]
        cmd: browser::BrowserCmd,
    },
}

#[derive(Subcommand)]
enum SyncCmd {
    /// Write a bundle for another machine. Prints to stdout without --output.
    Export {
        /// Where to write it. Omit to print to stdout.
        #[arg(long, short, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Export this blueprint rather than the one on the search path.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Leave projects out. A project entry carries a local path and a git
        /// remote, which is the only machine-specific data in a bundle.
        #[arg(long)]
        no_projects: bool,
    },
    /// Print a bundle without importing it.
    Show {
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },
    /// Install a bundle's blueprint and record its projects. Converges nothing.
    Import {
        #[arg(value_name = "PATH")]
        path: PathBuf,
        /// Replace an existing blueprint that differs. The current one is kept
        /// alongside it as blueprint.toml.previous.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args)]
struct ApplyArgs {
    /// Read this blueprint instead of the usual search path.
    #[arg(long, value_name = "PATH")]
    file: Option<PathBuf>,
    /// Report exactly what would change and perform none of it.
    ///
    /// The plan is computed once, so this prints the same steps a live run
    /// executes — it is a report, not a rehearsal of a different code path.
    #[arg(long)]
    dry_run: bool,
    /// Emit the plan as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum BlueprintCmd {
    /// Print the blueprint, where it came from, and when it was last applied.
    Show {
        /// Read this file instead of the usual search path.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// How the machine currently differs from the blueprint.
    ///
    /// Exits 0 when converged and 1 when there is drift `rime apply` could
    /// close, so it reads like `diff(1)` in a script. Changes that cannot be
    /// converged at all — a Gaming edition asked of a Daily machine — are
    /// reported but do not set the exit code, because no number of `apply`
    /// runs would ever clear them.
    Diff {
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Write a commented starting blueprint to ~/.config/rime/blueprint.toml.
    ///
    /// Every section arrives commented out, so the new file manages nothing
    /// until it is edited.
    Init {
        /// Overwrite an existing blueprint.
        #[arg(long)]
        force: bool,
    },
    /// Replace the blueprint with one supplied as JSON on stdin.
    ///
    /// The write path for §10's GUI editor. Without it the shell would have to
    /// author TOML itself, which means a second implementation of the schema
    /// that drifts the first time a field is added — and the lossless
    /// round-trip is the property the whole design rests on.
    ///
    /// The JSON goes through the same normalise + validate as a hand-edited
    /// file, so what the editor writes is indistinguishable from what a human
    /// types, and an invalid one is refused with the same messages. It writes
    /// desired state only: it converges nothing, never touches the generated
    /// applied-state file, and does not escalate.
    ///
    ///     rime blueprint show --json | jq … | rime blueprint set --json -
    Set {
        /// Read the blueprint as JSON from this source. Only `-` (stdin) is
        /// supported: a path would invite passing the live blueprint's own
        /// path and truncating it mid-read.
        #[arg(long, value_name = "-")]
        json: Option<String>,
    },
}

#[derive(Subcommand)]
enum PkgCmd {
    /// What is installed, and what came in as a dependency.
    List,
    /// Extension state: what it was built for, whether it is merged.
    Status,
    /// The full machine-readable record of the last build.
    Info,
    /// Re-resolve every package against the repositories. Requires root.
    Upgrade,
    /// Rebuild for the running OS version. Requires root.
    Rebuild {
        /// Do nothing unless the extension no longer matches the booted OS.
        #[arg(long)]
        if_needed: bool,
    },
    /// Restore the previous extension. Requires root.
    Rollback,
    /// Check the installed extension against its recorded checksum.
    Verify,
    /// Convert rpm-ostree layered packages into Rime packages, so that OS
    /// updates work again without losing the software. Requires root.
    Adopt,
}

/// `rime env <verb>` — the capsule surface (§8).
///
/// A separate enum rather than a raw argument passthrough so that `rime env
/// --help` documents the real thing and a typo is caught before a process is
/// spawned. The engine still owns every decision; this only builds its argv.
#[derive(Subcommand)]
enum EnvCmd {
    /// Create a capsule.
    ///
    /// A name that is also an image alias (fedora, ubuntu, arch, debian,
    /// python, cuda, rocm) brings that alias's image and device profile with
    /// it, so `rime env create cuda` is a capsule that can see the GPU.
    Create {
        #[arg(value_name = "NAME")]
        name: String,
        /// Any container image reference, instead of the alias's default.
        #[arg(long, value_name = "REF")]
        image: Option<String>,
        /// Device access: nvidia (host driver passthrough), amd (/dev/kfd and
        /// the render group, for ROCm), hw (USB buses, for hardware work), or
        /// none. Defaults to none — a capsule holding a device open is a
        /// capsule that stops the machine suspending.
        #[arg(long, value_name = "PROFILE")]
        gpu: Option<String>,
        /// Give the capsule its own home directory instead of sharing yours.
        /// For ecosystems whose caches litter $HOME badly enough to contain.
        #[arg(long, value_name = "DIR")]
        home: Option<String>,
    },
    /// Capsules on this machine, with their image and device profile.
    List {
        #[arg(long)]
        json: bool,
    },
    /// The full record for one capsule: image, digest, profile, package manager.
    Info {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Open an interactive shell in a capsule.
    Enter {
        #[arg(value_name = "NAME")]
        name: String,
        /// Run this instead of a login shell.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Run one command in a capsule with no TTY. For scripts and agents.
    Exec {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Install packages with the capsule's own package manager.
    ///
    /// This is what `rime install --source capsule` routes to: software that
    /// exists only as a package for a distribution Rime is not.
    Install {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(required = true, value_name = "PACKAGE")]
        packages: Vec<String>,
    },
    /// Remove a capsule. Only ones Rime created, unless --force.
    Rm {
        #[arg(value_name = "NAME")]
        name: String,
        /// Say where a custom home directory is rather than reporting it gone.
        #[arg(long)]
        keep_home: bool,
        /// Remove a container Rime has no record of.
        #[arg(long)]
        force: bool,
    },
    /// The image aliases and what they resolve to on this release.
    Images,
    /// Put a GUI application from a capsule into the host's launcher (§8).
    ///
    /// `distrobox-export` runs INSIDE the capsule and writes the .desktop file
    /// into your own `~/.local/share/applications`, so this needs no root and
    /// cannot raise an authentication prompt. `rime env rm` takes the launcher
    /// entry with it.
    Export {
        #[arg(value_name = "NAME")]
        name: String,
        /// The application as the capsule knows it — a bare name, not a path.
        #[arg(value_name = "APPLICATION")]
        app: String,
    },
    /// Take an exported application back out of the host's launcher.
    Unexport {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(value_name = "APPLICATION")]
        app: String,
    },
    /// What a capsule has exported, as distrobox sees it and as Rime recorded it.
    Exports {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Make a capsule that provides a language, and record that it does.
    ///
    /// This is what `rime apply` runs for the blueprint's `[development]
    /// languages`. A toolchain goes into a capsule, never onto the read-only
    /// host — that is the whole point of §8. The language is recorded only
    /// after the toolchain answers from inside the capsule.
    Provision {
        #[arg(value_name = "LANGUAGE")]
        language: String,
    },
    /// The language table: which capsule provides what, and from which packages.
    Languages,
}

/// `rime plugin <verb>` — the OS side of §16's plugin platform.
///
/// A separate enum rather than an argument passthrough, for the same reason
/// `EnvCmd` is one: `rime plugin --help` documents the real surface and a typo
/// is caught before a process is spawned.
#[derive(Subcommand)]
enum FirewallCmd {
    /// What the policy is, and which exceptions you have added.
    Status {
        /// Report as JSON, for a program rather than a person.
        ///
        /// Rime Shell's Firewall settings page reads this. It used to parse
        /// the helper's PROSE, and that broke the first time a line moved —
        /// silently, because a sentence has no shape to fail against. The
        /// contract the helper answers with is written at `cmd_status_json`
        /// in `files/system/libexec/rime-firewall`.
        ///
        /// This flag has to exist HERE and not only in the helper: `rime` is
        /// clap, `firewall_argv` rebuilds the helper's argv verb by verb from
        /// typed fields, and a `--json` clap does not model is rejected before
        /// any process is spawned. The helper growing the flag on its own
        /// would have left `rime firewall status --json` failing with
        /// "unexpected argument" while the helper it wraps supported it.
        #[arg(long)]
        json: bool,
    },
    /// The services you can open, by name.
    List,
    /// Open one service on every interface. Requires root.
    Allow {
        #[arg(value_name = "SERVICE")]
        name: String,
    },
    /// Close one again. Requires root.
    Deny {
        #[arg(value_name = "SERVICE")]
        name: String,
    },
    /// Reapply the recorded exceptions. Requires root.
    Reload,
}

/// The areas `rime devices` can report on.
///
/// A `ValueEnum` rather than a free string so a misspelling is answered by clap
/// with the list of real areas, before anything is executed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum DeviceArea {
    /// Printers, CUPS queues, mDNS discovery, and sharing one from here.
    Print,
    /// Scanners, and why the list may not arrive at all.
    Scan,
    /// SMB, NFS, WebDAV, MTP, cameras — and whether they exist outside GTK.
    Share,
    /// SD cards, USB disks, auto-mounting, and the seat it needs.
    Media,
    /// Links, captive portals, VPN, WireGuard, enterprise Wi-Fi, hotspot.
    Network,
    /// Adapters, radio blocks, paired devices, headset codecs.
    Bluetooth,
    /// Hotplug, Thunderbolt authorisation, USB-C ports and partners.
    Dock,
    /// Every area.
    All,
}

/// `rime skill <verb>` — P1-025's inventory.
///
/// Two verbs, and the split is the one `rime mcp` uses: `list` is the readout,
/// `audit` is the same measurement reduced to what is wrong with it and an
/// exit status a script can branch on.
#[derive(Subcommand)]
enum SkillCmd {
    /// Every skill, with its origin, digest and type.
    ///
    /// Exits non-zero if a skills directory could not be read — an incomplete
    /// inventory is not a successful one, because the count it prints is the
    /// number somebody would rely on to say nothing unexpected is installed.
    List {
        #[arg(long)]
        json: bool,
    },
    /// What is wrong: unreadable directories, missing manifests, links out of
    /// a skill, and every skill that ships a program.
    ///
    /// Exits non-zero when there is a problem.
    Audit {
        #[arg(long)]
        json: bool,
    },
}

/// `rime provenance <verb>` — P1-026.
///
/// `show` measures; `record` is the only thing here that writes, and it writes
/// nothing but Rime's own baselines. Recording is deliberately a separate verb
/// rather than something `show` does on first sight: a check that silently
/// adopted whatever it found as the truth could never report a change, because
/// the change would become the new baseline before anybody read it.
#[derive(Subcommand)]
enum ProvenanceCmd {
    /// Every marketplace and installed plugin: its origin, its revision, the
    /// digest of its files, whether that digest still matches the recorded
    /// one, and what confines each kind of executable content it ships.
    ///
    /// Exits non-zero when there is a finding, or when the report could not be
    /// completed — an inventory that could not read a registry is not a
    /// machine with no plugins.
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Record the current digest of every installed plugin as the baseline
    /// that later runs compare against.
    ///
    /// A tree that could not be hashed is not recorded, and a tree that had
    /// CHANGED since the last record is named on stderr as it is overwritten —
    /// re-recording is how a real finding gets erased, so it never happens
    /// quietly.
    Record,
}

#[derive(Subcommand)]
enum PluginCmd {
    /// Installed plugins, whether each one is valid, and why not.
    List {
        #[arg(long)]
        json: bool,
    },
    /// One plugin in full: its grant, its permissions, or its refusal reason.
    Info {
        #[arg(value_name = "ID")]
        id: String,
    },
    /// Move a plugin into the directory the shell scans.
    ///
    /// The shell scans exactly one directory and has no allowlist file, so this
    /// is a directory move — which is what actually takes effect against the
    /// shipped shell. It takes effect at the next shell start; nothing here can
    /// load a plugin into a running shell.
    Enable {
        #[arg(value_name = "ID")]
        id: String,
    },
    /// Move a plugin out of the directory the shell scans.
    ///
    /// Nothing is deleted and no file is rewritten. The running shell keeps a
    /// plugin it has already loaded until it restarts.
    Disable {
        #[arg(value_name = "ID")]
        id: String,
    },
}

#[derive(Subcommand)]
enum RepoCmd {
    /// List enabled and disabled repositories.
    List,
    /// Enable a Fedora COPR project (OWNER/PROJECT). Requires root.
    EnableCopr {
        #[arg(value_name = "OWNER/PROJECT")]
        project: String,
    },
    /// Disable a previously enabled Fedora COPR project. Requires root.
    DisableCopr {
        #[arg(value_name = "OWNER/PROJECT")]
        project: String,
    },
}

#[derive(Subcommand)]
enum FanCmd {
    /// Show every discovered fan, the active mode and the supported modes.
    Status,
    /// Switch mode: auto, max, manual, manual:<0-255> or curve.
    Mode { name: String },
    /// Manual mode at an explicit duty cycle (0-255).
    Pwm { value: u8 },
    /// Hand the fans back to firmware control.
    Restore {
        /// Write sysfs directly instead of going through rimed. This is the
        /// crash-safety path (`ExecStopPost=`) and needs root, not the daemon.
        #[arg(long)]
        local: bool,
    },
}

#[derive(Subcommand)]
enum GameCmd {
    /// Enter game mode, optionally pinning a process (and its children).
    Start {
        /// PID to move into the game cpuset.
        #[arg(long)]
        pid: Option<u32>,
        /// PID whose death ends the session. rimed watches it and releases
        /// game mode itself when it goes, which is the ONLY path that works
        /// once logind has deactivated the session: `rime game stop` is a
        /// polkit `allow_active=yes` action and an inactive session's call is
        /// refused (katana 2026-09-19, evidence §3.4). The owner is watched,
        /// not pinned — combine with `--pid` if it should also be in the
        /// cpuset. `rime-gaming-session` passes its own `$$` here.
        #[arg(long)]
        owner_pid: Option<u32>,
    },
    /// Leave game mode, restoring everything it changed.
    Stop,
    /// Show the session (or what one would look like).
    Status,
    /// Attach another PID to a running session.
    Attach { pid: u32 },
    /// Per-game profiles (roadmap §12): a stored composition of a mode, a tier
    /// and a fan mode, per title.
    ///
    /// Stored in `~/.config/rime/games.toml` — a separate user-owned file
    /// rather than a blueprint section, because the blueprint's contract is
    /// that no program rewrites it and `set` is a program that writes.
    Profile {
        #[command(subcommand)]
        cmd: gaming::ProfileCmd,
    },
}

#[derive(Args)]
struct UpdateArgs {
    /// Report what is available without downloading or staging anything.
    #[arg(long)]
    check: bool,
    /// Skip the firmware (fwupd) pass.
    #[arg(long)]
    skip_firmware: bool,
    /// Only run the firmware pass; leave the OS image alone.
    #[arg(long, conflicts_with = "skip_firmware")]
    firmware_only: bool,
    /// Skip refreshing packages installed with `rime install`.
    #[arg(long)]
    skip_packages: bool,
    /// Skip updating Flatpak applications.
    #[arg(long)]
    skip_flatpak: bool,
    /// Update even though the last one left this machine with a regression.
    ///
    /// §26's rollout stop refuses a second update on a machine that came back
    /// from the first one broken, because that is how one bad release becomes
    /// two. This is the way past it when you know better — for instance when
    /// the fix is in the release being held.
    #[arg(long)]
    force: bool,
    /// Deploy the next image even though its signature does not verify.
    ///
    /// §27's gate refuses an update whose image the machine cannot verify —
    /// see `docs/trust-enforcement.md` for what "cannot" covers and how to
    /// change it permanently. This is the one-off way past it.
    ///
    /// Deliberately not `--force`: that is §26's rollout stop, for a machine
    /// that came back from its last update broken. Working around a health
    /// stop must not silently stop checking signatures.
    #[arg(long)]
    allow_unverified: bool,
    /// Keep ostree's per-object fsync on during the pull. Roughly halves update
    /// speed (measured: ~8 MiB/s with it, ~14.6 without, because 179k objects at
    /// 2.98 ms of fsync each outweighs the download itself) in exchange for
    /// durability if the machine loses power mid-update.
    #[arg(long)]
    fsync: bool,
}

/// `rime shell <verb>` — the surfaces Rime Shell exposes over IPC.
///
/// Each variant maps to one `(target, function)` pair. Names are the
/// user-facing vocabulary ("launcher", "settings"), not the shell's internal
/// target strings, so the IPC surface can be renamed without breaking every
/// keybind on every machine.
#[derive(Subcommand)]
enum ShellCmd {
    /// Toggle the app launcher.
    Launcher,
    /// Toggle the dashboard. Optionally on a specific page.
    Dashboard {
        /// home | stats | kanban | launcher | config
        #[arg(value_name = "PAGE")]
        page: Option<String>,
    },
    /// Open the settings window, optionally at a page (appearance, layout,
    /// data, keybinds, misc). Run `rime shell settings --list` for the live
    /// list.
    Settings {
        #[arg(value_name = "PAGE")]
        page: Option<String>,
        /// Print the page names the running shell actually offers.
        ///
        /// Conflicts with PAGE and --close rather than silently taking
        /// precedence: `settings --close --list` had no obvious meaning, and
        /// quietly honouring one of them is how a script ends up doing the
        /// opposite of what it says.
        #[arg(long, conflicts_with_all = ["page", "close"])]
        list: bool,
        /// Close it instead of toggling.
        #[arg(long, conflicts_with = "page")]
        close: bool,
    },
    /// Lock the session.
    Lock,
    /// Toggle the notification centre.
    Notifications,
    /// Toggle clipboard history.
    Clipboard,
    /// Toggle the wallpaper picker.
    Wallpaper,
    /// Toggle the power menu.
    Power,
    /// Toggle the desktop context menu (what a right-click on the desktop
    /// opens). Replaces the compositor's own root menu; the same QML surface
    /// serves all three sessions.
    Menu,
    /// Toggle the audio panel (output, input or the app mixer).
    Audio {
        /// out | in | mixer
        #[arg(value_name = "WHICH", default_value = "out")]
        which: String,
    },
    /// Toggle the network panel on a given tab.
    Network {
        /// wifi | bluetooth | vpn | hotspot
        #[arg(value_name = "TAB", default_value = "wifi")]
        tab: String,
    },
    /// Toggle focus mode.
    Focus,
    /// Start the screen-recorder setup strip.
    Record,
    /// Start or stop push-to-talk.
    ///
    /// A toggle rather than hold-to-talk because niri has no bind that fires
    /// on key release, so press-and-hold would work on Hyprland and labwc and
    /// do nothing useful on niri. The shell shows a microphone indicator
    /// naming the session the words are going to, and stops on its own after
    /// ninety seconds.
    Voice,
    /// Drive the Alt-Tab window switcher: next | prev | commit | cancel.
    ///
    /// Bound by the compositor, not typed: ALT+Tab runs `next`, releasing ALT
    /// runs `commit`. `/usr/libexec/rime-switcher` is the wrapper the keybinds
    /// actually name — it skips the IPC entirely when no switcher is open,
    /// because the release binding fires on every ALT release.
    Switcher {
        /// next | prev | commit | cancel
        #[arg(value_name = "ACTION")]
        action: String,
    },
    /// List every target this wrapper knows, with the IPC call behind it.
    List,
    /// Call an arbitrary target/function, for anything not covered above.
    Ipc {
        #[arg(value_name = "TARGET")]
        target: String,
        #[arg(value_name = "FUNCTION", default_value = "toggle")]
        function: String,
        /// Extra positional arguments passed through to the handler.
        #[arg(value_name = "ARG")]
        args: Vec<String>,
    },
}

#[derive(Args)]
struct MetricsArgs {
    /// Emit machine-readable JSON instead of an aligned table.
    #[arg(long)]
    json: bool,
    /// Keep printing a new sample every INTERVAL seconds until interrupted.
    ///
    /// With --json this produces one JSON object per line (JSON Lines), which is
    /// the shape a log shipper or `jq --unbuffered` wants.
    #[arg(long, value_name = "INTERVAL", num_args = 0..=1, default_missing_value = "2")]
    stream: Option<f64>,
}

#[derive(Args)]
struct BatteryArgs {
    /// Enable travel mode (tighten charge to a storage window).
    #[arg(long)]
    travel: bool,
    /// Set charge start/stop thresholds (percent).
    #[arg(long, num_args = 2, value_names = ["START", "END"])]
    thresholds: Option<Vec<u8>>,
    /// Begin a battery calibration cycle.
    #[arg(long)]
    calibrate: bool,
}

/// Which verbs refuse to run without root, and under what name.
///
/// A function rather than a `match` inside `main` so the privilege set is an
/// assertion instead of a reading exercise. It covers exactly these and not
/// the whole CLI: the desktop's power tab drives `rime tier` as the session
/// user, and every read-only verb has to stay usable without a password.
/// See `ops::require_root` for what the refusal says.
fn privileged_verb(cmd: &Cmd) -> Option<&'static str> {
    match cmd {
        Cmd::Update(_) => Some("update"),
        Cmd::Rollback => Some("rollback"),
        Cmd::Pin => Some("pin"),
        // Only `set`. It is `bootc switch`, which rewrites the deployment
        // origin, and on the backwards direction `ostree admin pin` as well.
        // `status`, `list` and `report` read the origin file, /etc/machine-id
        // and /var/lib/rime — all world-readable — so gating them would put a
        // password in front of "which channel am I on", which is the question
        // somebody asks when they are already in trouble.
        Cmd::Channel { cmd: channel::ChannelCmd::Set { .. } } => Some("channel set"),
        // `--source capsule` writes nothing the system owns: it installs into
        // a rootless per-user container. Demanding root for it would be worse
        // than pointless — root has no capsules, so `sudo rime install
        // --source capsule` reports an empty list on every machine, and the
        // user who typed sudo because the CLI asked for it gets a refusal from
        // the engine instead of a package.
        Cmd::Install {
            source: Some(s), ..
        } if s == "capsule" => None,
        // Package verbs that write: they build an extension into /var/lib and
        // ask systemd to re-merge /usr. The read-only ones (list/status/info/
        // verify), `search` and `resolve` stay usable as an ordinary user on
        // purpose.
        Cmd::Install { .. } => Some("install"),
        Cmd::Remove { .. } => Some("remove"),
        Cmd::Pkg {
            cmd: PkgCmd::Upgrade,
        } => Some("pkg upgrade"),
        Cmd::Pkg {
            cmd: PkgCmd::Rebuild { .. },
        } => Some("pkg rebuild"),
        Cmd::Pkg {
            cmd: PkgCmd::Rollback,
        } => Some("pkg rollback"),
        Cmd::Pkg {
            cmd: PkgCmd::Adopt,
        } => Some("pkg adopt"),
        // `fan restore --local` writes sysfs directly instead of asking rimed;
        // it is the crash-safety path (ExecStopPost=) and needs real privileges.
        // Every other fan verb goes through the daemon and must stay usable.
        Cmd::Fan {
            cmd: Some(FanCmd::Restore { local: true }),
        } => Some("fan restore --local"),
        // `rime env` is deliberately absent: capsules are rootless per-user
        // containers, and running one as root would put its images under
        // /var/lib/containers, share it between every account, and need an
        // authentication prompt to open a shell.
        _ => None,
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Root-only verbs bail HERE — before any sysfs probe, D-Bus connect or
    // subprocess — so an unprivileged `rime update` costs nothing and answers
    // instantly with the command to run instead.
    if let Some(verb) = privileged_verb(&cli.command) {
        if let Err(code) = ops::require_root(verb) {
            std::process::exit(code);
        }
    }

    let code = match cli.command {
        Cmd::Status => cmd_status().await,
        // The agent verbs are a blocking client over the per-user runtime's
        // Unix socket, not a D-Bus call, and `attach` deliberately blocks for
        // as long as the user stays attached.
        Cmd::Agent { cmd } => agent::agent(cmd),
        Cmd::Project { cmd } => agent::project_cmd(cmd),
        // Reads the task file, the capsule engine's records, the project's
        // checkpoints and the agent runtime's session list; writes only the
        // task file and the task's own state file. No D-Bus, no root, and
        // nothing that can raise a prompt — routed here, before anything
        // connects to the system bus, for the reason `rime ai` is.
        Cmd::Task(args) => task::run(args),
        Cmd::Request { cmd } => request::main(cmd),
        Cmd::Cloudflare { cmd } => cloudflare::main(cmd),
        Cmd::Backup { cmd } => backup::main(cmd),
        Cmd::Account { cmd } => account::main(cmd),
        Cmd::Secret { cmd } => secret::main(cmd),
        Cmd::Mcp { cmd } => mcp::main(cmd),
        Cmd::GitShim { args } => gitshim::main(args),
        // Read-only, so no root gate: seeing what the machine should be must
        // not require privilege. `rime apply` is the verb that changes things,
        // and it converges only the privilege domain it is already in.
        Cmd::Blueprint { cmd } => match cmd {
            BlueprintCmd::Show { file, json } => blueprint::cmd_show(file.as_deref(), json),
            BlueprintCmd::Diff { file, json } => blueprint::cmd_diff(file.as_deref(), json),
            BlueprintCmd::Init { force } => blueprint::cmd_init(force),
            BlueprintCmd::Set { json } => blueprint::cmd_set(json.as_deref() == Some("-")),
        },
        // Deliberately NOT in the root-only list above. `apply` is a mixed
        // verb: it converges the domain it is in and reports the other, so
        // gating the whole command on root would make the user half — colour
        // scheme, agent defaults — reachable only by running it as the wrong
        // user, which is precisely the mistake the domain split exists to
        // prevent.
        Cmd::Apply(args) => blueprint::cmd_apply(args.file.as_deref(), args.dry_run, args.json),
        // `sync` writes only the user's own blueprint and project records, so
        // it needs no privilege and must not ask for any.
        Cmd::Sync { cmd } => match cmd {
            SyncCmd::Export {
                output,
                file,
                no_projects,
            } => blueprint::cmd_sync_export(file.as_deref(), output.as_deref(), no_projects),
            SyncCmd::Show { path } => blueprint::cmd_sync_show(&path),
            SyncCmd::Import { path, force } => blueprint::cmd_sync_import(&path, force),
        },
        Cmd::Tier { name } => cmd_tier(name).await,
        Cmd::Profile => cmd_profile().await,
        Cmd::Battery(args) => cmd_battery(args).await,
        Cmd::Fan { cmd } => cmd_fan(cmd.unwrap_or(FanCmd::Status)).await,
        // `profile` is intercepted HERE rather than inside `cmd_game`, and the
        // reason is the guard: `cmd_game` connects to the system bus as its
        // first act, before it looks at the verb at all. Routing
        // `profile apply` through it would put a bus connection ahead of
        // RIME_MODE_NO_APPLY, so on a machine with no bus the command would
        // fail for the wrong reason and the guard's ordering proof would be
        // vacuous. `rime mode set` has the same rule; this is the same rule.
        Cmd::Game { cmd } => match cmd {
            GameCmd::Profile { cmd } => gaming::profile_main(cmd).await,
            other => cmd_game(other).await,
        },
        // Read-only by default and deliberately absent from the privileged set:
        // `mode set` mutates through rimed's polkit-authorised D-Bus API as the
        // session user, exactly as `rime tier` does.
        Cmd::Mode { cmd } => mode::main(cmd.unwrap_or(mode::ModeCmd::Status)).await,
        Cmd::Workload(args) => mode::workload_main(args),
        Cmd::Perf(args) => mode::perf_main(args),
        // Read-only, like `perf` and `workload`, and for the same reason it is
        // not in the privileged set: it measures and reports.
        Cmd::Gaming(args) => gaming::gaming_main(args),
        // Also read-only, and deliberately not in the privileged set even
        // though the boot chain is the most privileged thing on the machine.
        // Reading the ESP does need root, and `status` reports that as
        // "unavailable, and why" rather than demanding a password to answer
        // "what verified my boot".
        Cmd::Boot { cmd } => boot::boot_main(cmd),
        Cmd::Lid { cmd } => lid::main(cmd),
        Cmd::Permissions { cmd } => permissions::main(cmd),
        // Same shape as `boot`, and for the same reason: the honest answer to
        // "is my operating system signed" must not cost a password, so the
        // offline half is file reads and `--verify` is the only path that
        // leaves the machine.
        // Read-only for `status`, and a dry run for `migrate` unless it is
        // given --commit. Every path it touches is in the user's own home, so
        // there is no root gate and nothing here can raise a prompt.
        // `status`, `list` and `report` are read-only and root-free. `set` is
        // `bootc switch`, which is in the privileged set below beside Update,
        // Rollback and Pin.
        Cmd::Channel { cmd } => channel::main(cmd),
        Cmd::Qualify { cmd } => qualify::main(cmd),
        Cmd::Storage { cmd } => storage::main(cmd),
        Cmd::Firmware { cmd } => firmware::main(cmd),
        Cmd::Schema { cmd } => schema::main(cmd),
        Cmd::Trust(args) => trust::main(args),
        // Read-only except for `add`/`remove`/`probe`, which write only the
        // registry and the probe cache in the user's own home. Nothing here
        // touches rimed or needs root.
        // Routed here, before anything connects to the system bus: `rime ai`
        // talks to a per-user daemon and to the model store, never to `rimed`,
        // so a system-bus connection ahead of it would be a dependency the
        // feature does not have — and on a machine with no `rimed` it would be
        // a failure the user cannot act on.
        Cmd::Ai { cmd } => ai::main(cmd),
        Cmd::Host(args) => host::run(args),
        // Local by default; `--on` is the only thing that makes any of these
        // touch the network. None of them needs root: they run ssh as the
        // invoking user and write nothing outside the user's own home.
        Cmd::Build(args) => dispatch::build(args),
        Cmd::Send(args) => dispatch::send(args),
        Cmd::Open(args) => dispatch::open(args),
        Cmd::Fingerprint => cmd_fingerprint(),
        Cmd::Pin => ops::pin(),
        Cmd::Rollback => ops::rollback(),
        Cmd::Update(args) => ops::update(ops::UpdateOptions {
            check: args.check,
            skip_firmware: args.skip_firmware,
            firmware_only: args.firmware_only,
            keep_fsync: args.fsync,
            skip_packages: args.skip_packages,
            skip_flatpak: args.skip_flatpak,
            force: args.force,
            allow_unverified: args.allow_unverified,
        }),
        Cmd::Shell { cmd } => cmd_shell(cmd),
        Cmd::Metrics(args) => cmd_metrics(args).await,
        Cmd::Doctor { json } => cmd_doctor(json).await,
        // Read-only and subprocess-free, so deliberately not in the privileged
        // set: "what state is my machine in, and how do I get back" must never
        // cost a password. `repair` converges the domain it is already in and
        // reports the other, and `reset` refuses to run as root outright —
        // root's home is not the user's, so a `sudo` run would reset the wrong
        // account while reporting success.
        Cmd::Recover { cmd } => recover::main(cmd),
        // Unprivileged for the same structural reason `rime env` is: a
        // disposable capsule is a rootless per-user container.
        Cmd::Disposable { cmd } => ops::disposable(&disposable::argv(cmd)),
        // Accounts. The engine decides everything and refuses what it
        // must; this only builds the argv, and `user::argv` pins it.
        Cmd::User { cmd } => ops::user(&user::argv(cmd)),
        Cmd::Vm { cmd } => ops::vm(&vm::argv(cmd)),
        Cmd::Browser { cmd } => ops::browser(&browser::argv(cmd)),
        Cmd::Changelog => ops::changelog(),
        Cmd::Install {
            packages,
            no_weak_deps,
            enable_repo,
            allow_unsigned,
            source,
            env,
        } => ops::pkg(&install_argv(
            packages,
            no_weak_deps,
            enable_repo,
            allow_unsigned,
            source,
            env,
        )),
        Cmd::Resolve { name } => ops::pkg(&["resolve".to_string(), name]),
        Cmd::Remove { packages } => {
            let mut argv = vec!["remove".to_string()];
            argv.extend(packages);
            ops::pkg(&argv)
        }
        Cmd::Search { terms } => {
            let mut argv = vec!["search".to_string()];
            argv.extend(terms);
            ops::pkg(&argv)
        }
        Cmd::Repo { cmd } => {
            let argv = match cmd {
                RepoCmd::List => vec!["repo-list".into()],
                RepoCmd::EnableCopr { project } => vec!["repo-enable-copr".into(), project],
                RepoCmd::DisableCopr { project } => vec!["repo-disable-copr".into(), project],
            };
            ops::pkg(&argv)
        }
        Cmd::Pkg { cmd } => {
            let argv: Vec<String> = match cmd {
                PkgCmd::List => vec!["list".into()],
                PkgCmd::Status => vec!["status".into()],
                PkgCmd::Info => vec!["info".into()],
                PkgCmd::Upgrade => vec!["upgrade".into()],
                PkgCmd::Rebuild { if_needed } => {
                    let mut a = vec!["rebuild".to_string()];
                    if if_needed {
                        a.push("--if-needed".into());
                    }
                    a
                }
                PkgCmd::Rollback => vec!["rollback".into()],
                PkgCmd::Verify => vec!["verify".into()],
                PkgCmd::Adopt => vec!["adopt".into()],
            };
            ops::pkg(&argv)
        }
        Cmd::Env { cmd } => ops::env(&env_argv(cmd)),
        Cmd::Firewall { cmd } => ops::firewall(&firewall_argv(cmd)),
        Cmd::Devices { area } => ops::devices(&devices_argv(area)),
        Cmd::Remote { cmd } => remote::remote(cmd),
        Cmd::Plugin { cmd } => ops::plugin(&plugin_argv(cmd)),
        Cmd::Skill { cmd } => match cmd {
            SkillCmd::List { json } => skill::list(json),
            SkillCmd::Audit { json } => skill::audit(json),
        },
        Cmd::Provenance { cmd } => match cmd {
            ProvenanceCmd::Show { json } => provenance::show(json),
            ProvenanceCmd::Record => provenance::record(),
        },
    };
    std::process::exit(code);
}

/// Build the engine argv for `rime install`.
///
/// Split out of `main` so the mapping can be pinned by a test: the engine is a
/// separate process, so a dropped or misspelled flag here is not a compile error
/// — it is a silent policy change. `--allow-unsigned` in particular decides
/// whether an unverifiable RPM is refused or installed.
fn install_argv(
    packages: Vec<String>,
    no_weak_deps: bool,
    enable_repo: Vec<String>,
    allow_unsigned: bool,
    source: Option<String>,
    env: Option<String>,
) -> Vec<String> {
    let mut argv = vec!["install".to_string()];
    argv.extend(packages);
    if no_weak_deps {
        argv.push("--no-weak-deps".to_string());
    }
    for repo in enable_repo {
        argv.push(format!("--enable-repo={repo}"));
    }
    if allow_unsigned {
        argv.push("--allow-unsigned".to_string());
    }
    // The engine validates the value and refuses an unknown one. Not
    // re-validated here: two lists of legal sources would be one list too
    // many, and the engine's is the one that decides.
    if let Some(s) = source {
        argv.push(format!("--source={s}"));
    }
    if let Some(e) = env {
        argv.push(format!("--env={e}"));
    }
    argv
}

/// Build the engine argv for `rime env`.
///
/// Split out for the same reason `install_argv` is: the engine is a separate
/// process, so a dropped flag is not a compile error but a silent policy
/// change. `--gpu` decides whether a capsule can see the GPU at all, and
/// `--force` decides whether `rm` will destroy a container Rime did not
/// create.
///
/// The `--` before a trailing command is not cosmetic. Without it the engine
/// cannot tell `rime env exec box -- ls -l` (run `ls -l`) from a flag of its
/// own, and clap has already stripped the separator the user typed.
fn env_argv(cmd: EnvCmd) -> Vec<String> {
    match cmd {
        EnvCmd::Create {
            name,
            image,
            gpu,
            home,
        } => {
            let mut a = vec!["create".to_string(), name];
            if let Some(i) = image {
                a.push(format!("--image={i}"));
            }
            if let Some(g) = gpu {
                a.push(format!("--gpu={g}"));
            }
            if let Some(h) = home {
                a.push(format!("--home={h}"));
            }
            a
        }
        EnvCmd::List { json } => {
            let mut a = vec!["list".to_string()];
            if json {
                a.push("--json".to_string());
            }
            a
        }
        EnvCmd::Info { name } => vec!["info".to_string(), name],
        EnvCmd::Enter { name, command } => {
            let mut a = vec!["enter".to_string(), name];
            if !command.is_empty() {
                a.push("--".to_string());
                a.extend(command);
            }
            a
        }
        EnvCmd::Exec { name, command } => {
            let mut a = vec!["exec".to_string(), name, "--".to_string()];
            a.extend(command);
            a
        }
        EnvCmd::Install { name, packages } => {
            let mut a = vec!["install".to_string(), name];
            a.extend(packages);
            a
        }
        EnvCmd::Rm {
            name,
            keep_home,
            force,
        } => {
            let mut a = vec!["rm".to_string(), name];
            if keep_home {
                a.push("--keep-home".to_string());
            }
            if force {
                a.push("--force".to_string());
            }
            a
        }
        EnvCmd::Images => vec!["images".to_string()],
        EnvCmd::Export { name, app } => vec!["export".to_string(), name, app],
        EnvCmd::Unexport { name, app } => vec!["unexport".to_string(), name, app],
        EnvCmd::Exports { name } => vec!["exports".to_string(), name],
        EnvCmd::Provision { language } => vec!["provision".to_string(), language],
        EnvCmd::Languages => vec!["languages".to_string()],
    }
}

/// Kept a pure function, like `plugin_argv`, so the argv this hands a
/// root-run helper can be asserted without running it.
fn firewall_argv(cmd: FirewallCmd) -> Vec<String> {
    match cmd {
        FirewallCmd::Status { json } => {
            let mut a = vec!["status".to_string()];
            if json {
                a.push("--json".to_string());
            }
            a
        }
        FirewallCmd::List => vec!["list".to_string()],
        FirewallCmd::Allow { name } => vec!["allow".to_string(), name],
        FirewallCmd::Deny { name } => vec!["deny".to_string(), name],
        FirewallCmd::Reload => vec!["reload".to_string()],
    }
}

/// Pure, like `firewall_argv`: the helper is a separate process, so a wrong
/// word here is not a compile error but a diagnostic that reports the wrong
/// area — or, with no default, no area at all.
fn devices_argv(area: Option<DeviceArea>) -> Vec<String> {
    vec![match area.unwrap_or(DeviceArea::All) {
        DeviceArea::Print => "print",
        DeviceArea::Scan => "scan",
        DeviceArea::Share => "share",
        DeviceArea::Media => "media",
        DeviceArea::Network => "network",
        DeviceArea::Bluetooth => "bluetooth",
        DeviceArea::Dock => "dock",
        DeviceArea::All => "all",
    }
    .to_string()]
}

fn plugin_argv(cmd: PluginCmd) -> Vec<String> {
    match cmd {
        PluginCmd::List { json } => {
            let mut a = vec!["list".to_string()];
            if json {
                a.push("--json".to_string());
            }
            a
        }
        PluginCmd::Info { id } => vec!["info".to_string(), id],
        PluginCmd::Enable { id } => vec!["enable".to_string(), id],
        PluginCmd::Disable { id } => vec!["disable".to_string(), id],
    }
}

fn cmd_fingerprint() -> i32 {
    let v = LocalView::detect();
    print!("{}", ops::render_fingerprint(&v.fingerprint, &v.selection));
    0
}

async fn cmd_status() -> i32 {
    let v = LocalView::detect();
    print!("{}", ops::render_fingerprint(&v.fingerprint, &v.selection));

    // §27: "`rime status` should surface trust state clearly." Placed before
    // the daemon section because it must appear on a machine where rimed is
    // not running — that branch returns early, and a trust readout only the
    // healthy machines get is the wrong way round.
    //
    // Offline only. Nothing here contacts the registry, so `rime status` keeps
    // costing one set of file reads and cannot hang on a dead network.
    println!();
    print!(
        "{}",
        trust::render_block(&trust::offline_report(&trust::Roots::from_env()))
    );

    let conn = connect().await;
    let running = match &conn {
        Some(c) => daemon_running(c).await,
        None => false,
    };

    if !running {
        println!("\nrimed: not running — showing local dry-run view.\n");
        print!("{}", ops::render_tier_plans(v.active_profile()));
        return 0;
    }

    let conn = conn.unwrap();
    println!("\nDaemon (live):");
    if let Ok(p) = PowerProxy::new(&conn).await {
        print_kv("  tier", p.tier().await.ok());
        print_kv(
            "  on AC",
            p.on_ac_power().await.ok().map(|b| b.to_string()),
        );
        print_kv(
            "  auto-switch",
            p.auto_switch().await.ok().map(|b| b.to_string()),
        );
        if let Ok(tiers) = p.tiers().await {
            println!("  tiers        : {}", tiers.join(", "));
        }
    }
    if let Ok(b) = BatteryProxy::new(&conn).await {
        print_kv("  battery", b.status().await.ok());
        print_kv(
            "  capacity",
            b.capacity().await.ok().map(|c| format!("{c}%")),
        );
        if let (Ok(s), Ok(e)) = (b.charge_start().await, b.charge_end().await) {
            println!("  charge       : {s}-{e}");
        }
        print_kv(
            "  travel mode",
            b.travel_mode().await.ok().map(|b| b.to_string()),
        );
    }
    0
}

async fn cmd_tier(name: Option<String>) -> i32 {
    let v = LocalView::detect();
    let conn = connect().await;
    let running = match &conn {
        Some(c) => daemon_running(c).await,
        None => false,
    };

    match name {
        // Query mode.
        None => {
            if running {
                if let Ok(p) = PowerProxy::new(conn.as_ref().unwrap()).await {
                    let cur = p.tier().await.unwrap_or_default();
                    let tiers = p.tiers().await.unwrap_or_else(|_| Tier::all_ids());
                    for t in tiers {
                        println!("{} {}", if t == cur { "*" } else { " " }, t);
                    }
                    return 0;
                }
            }
            println!("rimed not running — tiers (local):");
            for t in Tier::ALL {
                println!("  {} [{}]", t.label(), t.as_str());
            }
            let d = &v.active_profile().defaults;
            println!("  default: AC -> {}, battery -> {}", d.ac, d.battery);
            0
        }
        // Set mode.
        Some(name) => {
            let tier: Tier = match name.parse() {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("rime: {e}");
                    return 2;
                }
            };
            if running {
                match PowerProxy::new(conn.as_ref().unwrap()).await {
                    Ok(p) => match p.set_tier(tier.as_str()).await {
                        Ok(()) => {
                            println!("rime: tier -> {tier}");
                            0
                        }
                        Err(e) => {
                            eprintln!("rime: SetTier failed: {e}");
                            1
                        }
                    },
                    Err(e) => {
                        eprintln!("rime: cannot reach rimed: {e}");
                        1
                    }
                }
            } else {
                eprintln!("rime: rimed not running — cannot apply '{tier}'. Dry-run plan:");
                for a in v.active_profile().plan_tier(tier) {
                    eprintln!("  - {}", a.describe());
                }
                1
            }
        }
    }
}

async fn cmd_profile() -> i32 {
    let v = LocalView::detect();
    let conn = connect().await;
    let running = match &conn {
        Some(c) => daemon_running(c).await,
        None => false,
    };

    if running {
        if let Ok(p) = ProfileProxy::new(conn.as_ref().unwrap()).await {
            println!("active : {}", p.active().await.unwrap_or_default());
            let class = p.class().await.unwrap_or_default();
            let device = p.device().await.unwrap_or_default();
            println!("class  : {}", if class.is_empty() { "(none)" } else { &class });
            println!(
                "device : {}",
                if device.is_empty() { "(none)" } else { &device }
            );
        }
    } else {
        let s = &v.selection;
        println!("active : {}", s.active);
        println!(
            "class  : {}",
            if s.class_or_empty().is_empty() { "(none)" } else { s.class_or_empty() }
        );
        println!(
            "device : {}",
            if s.device_or_empty().is_empty() { "(none)" } else { s.device_or_empty() }
        );
        println!("(rimed not running — resolved locally)");
    }

    let d = &v.active_profile().defaults;
    println!("\ndefaults: AC -> {}, battery -> {}", d.ac, d.battery);
    if let Some(c) = &v.active_profile().charge {
        println!("charge  : {}-{}", c.start, c.stop);
    }
    0
}

async fn cmd_battery(args: BatteryArgs) -> i32 {
    let conn = connect().await;
    let running = match &conn {
        Some(c) => daemon_running(c).await,
        None => false,
    };

    // Mutating verbs require the daemon.
    let mutating = args.travel || args.calibrate || args.thresholds.is_some();
    if mutating && !running {
        eprintln!("rime: rimed not running — cannot change battery settings.");
        return 1;
    }

    if running {
        let conn = conn.as_ref().unwrap();
        if let Ok(b) = BatteryProxy::new(conn).await {
            if let Some(t) = &args.thresholds {
                let (start, end) = (t[0], t[1]);
                return match b.set_charge_thresholds(start, end).await {
                    Ok(()) => {
                        println!("rime: charge thresholds -> {start}-{end}");
                        0
                    }
                    Err(e) => {
                        eprintln!("rime: SetChargeThresholds failed: {e}");
                        1
                    }
                };
            }
            if args.travel {
                return match b.set_travel_mode(true).await {
                    Ok(()) => {
                        println!("rime: travel mode enabled");
                        0
                    }
                    Err(e) => {
                        eprintln!("rime: SetTravelMode failed: {e}");
                        1
                    }
                };
            }
            if args.calibrate {
                return match b.calibrate().await {
                    Ok(()) => {
                        println!("rime: calibration cycle started");
                        0
                    }
                    Err(e) => {
                        eprintln!("rime: Calibrate failed: {e}");
                        1
                    }
                };
            }
            // No flags: show live battery.
            print_kv("battery ", b.status().await.ok());
            print_kv("capacity", b.capacity().await.ok().map(|c| format!("{c}%")));
            if let (Ok(s), Ok(e)) = (b.charge_start().await, b.charge_end().await) {
                println!("charge  : {s}-{e}");
            }
            print_kv(
                "travel  ",
                b.travel_mode().await.ok().map(|b| b.to_string()),
            );
            return 0;
        }
    }

    // Daemon-less read-only view, against whatever batteries this machine has.
    let inv = rimed_core::BatteryInventory::detect();
    let Some(bat) = inv.primary() else {
        println!("battery : (none — this machine has no battery)");
        println!("(rimed not running — read locally)");
        return 0;
    };
    println!("battery : {}", bat.read("status").unwrap_or_else(|| "Unknown".into()));
    println!("capacity: {}%", bat.read("capacity").unwrap_or_else(|| "?".into()));
    if inv.len() > 1 {
        println!("packs   : {}", inv.names().join(", "));
    }
    for b in &inv.batteries {
        let end = b.end_path.as_deref().and_then(read_abs);
        let start = b.start_path.as_deref().and_then(read_abs);
        match (start, end) {
            (Some(s), Some(e)) => println!("charge  : {} {s}-{e}", b.name),
            (None, Some(e)) => println!("charge  : {} stop at {e} (no start threshold)", b.name),
            _ => {}
        }
    }
    if !inv.supports_thresholds() {
        println!("charge  : not supported on this hardware");
    }
    println!("(rimed not running — read locally)");
    0
}

async fn cmd_fan(cmd: FanCmd) -> i32 {
    // `restore --local` deliberately skips every daemon check: it is the path
    // `rimed.service`'s ExecStopPost= takes after a crash, when there is no
    // daemon left to ask.
    if let FanCmd::Restore { local: true } = cmd {
        return fan_restore_locally();
    }

    let conn = connect().await;
    let running = match &conn {
        Some(c) => daemon_running(c).await,
        None => false,
    };
    let proxy = match (&conn, running) {
        (Some(c), true) => FanProxy::new(c).await.ok(),
        _ => None,
    };

    match cmd {
        FanCmd::Status => {
            match &proxy {
                Some(p) => {
                    let supported = p.supported().await.unwrap_or(false);
                    println!("mode      : {}", p.mode().await.unwrap_or_default());
                    println!("supported : {supported}");
                    if let Ok(modes) = p.modes().await {
                        println!("modes     : {}", modes.join(", "));
                    }
                    if let Ok(pwm) = p.pwm().await {
                        if pwm > 0 {
                            println!("pwm       : {pwm} ({}%)", (pwm as u32 * 100) / 255);
                        }
                    }
                    if let Ok(fans) = p.fans().await {
                        if fans.is_empty() {
                            println!("fans      : (none detected)");
                        }
                        for f in fans {
                            println!("  {}", render_fan(&f));
                        }
                    }
                }
                None => {
                    let v = LocalView::detect();
                    let cfg = v.active_profile().fan_config();
                    let inv = rimed_core::fan::FanInventory::discover(Path::new("/sys"), &cfg);
                    println!("rimed not running — reading fans locally.");
                    println!("supported : {}", inv.controllable());
                    println!("modes     : {}", inv.modes(&cfg).join(", "));
                    let readings = inv.read();
                    if readings.is_empty() {
                        println!("fans      : (none detected)");
                    }
                    for r in readings {
                        let mut parts = vec![r.id.clone()];
                        if let Some(rpm) = r.rpm {
                            parts.push(format!("{rpm} rpm"));
                        }
                        if let Some(p) = r.percent {
                            parts.push(format!("{p}%"));
                        }
                        if let Some(p) = r.pwm {
                            parts.push(format!("pwm {p}"));
                        }
                        if r.controllable {
                            parts.push("controllable".into());
                        }
                        println!("  {}", parts.join("  "));
                    }
                }
            }
            0
        }
        FanCmd::Mode { name } => match &proxy {
            Some(p) => match p.set_mode(&name).await {
                Ok(()) => {
                    println!("rime: fan mode -> {name}");
                    0
                }
                Err(e) => {
                    eprintln!("rime: SetMode failed: {e}");
                    1
                }
            },
            None => {
                eprintln!("rime: rimed not running — cannot change fan mode.");
                1
            }
        },
        FanCmd::Pwm { value } => match &proxy {
            Some(p) => match p.set_pwm(value).await {
                Ok(()) => {
                    println!("rime: fan pwm -> {value}");
                    0
                }
                Err(e) => {
                    eprintln!("rime: SetPwm failed: {e}");
                    1
                }
            },
            None => {
                eprintln!("rime: rimed not running — cannot set fan pwm.");
                1
            }
        },
        FanCmd::Restore { local: _ } => match &proxy {
            Some(p) => match p.restore_firmware().await {
                Ok(()) => {
                    println!("rime: fans restored to firmware control");
                    0
                }
                Err(e) => {
                    eprintln!("rime: RestoreFirmware failed: {e} — falling back to a local restore");
                    fan_restore_locally()
                }
            },
            // No daemon: still restore, directly. Never leave fans in whatever
            // state a dead daemon left them.
            None => fan_restore_locally(),
        },
    }
}

/// Write the fan-restore plan straight to sysfs. Root-only; honours
/// `RIMED_DRY_RUN=1`.
fn fan_restore_locally() -> i32 {
    let v = LocalView::detect();
    let cfg = v.active_profile().fan_config();
    let dry = rimed_core::dry_run_from_env();
    let writer = rimed_core::RealWriter::new(dry);
    let n = rimed_core::fan::restore_to_firmware(Path::new("/sys"), &cfg, &writer);
    if n == 0 {
        println!("rime: no controllable fan found — nothing to restore");
    } else {
        println!(
            "rime: fans handed back to firmware control ({n} action(s){})",
            if dry { ", dry-run" } else { "" }
        );
    }
    0
}

async fn cmd_game(cmd: GameCmd) -> i32 {
    let conn = connect().await;
    let running = match &conn {
        Some(c) => daemon_running(c).await,
        None => false,
    };
    let proxy = match (&conn, running) {
        (Some(c), true) => GameModeProxy::new(c).await.ok(),
        _ => None,
    };

    match cmd {
        GameCmd::Status => {
            match &proxy {
                Some(p) => {
                    println!("active    : {}", p.active().await.unwrap_or(false));
                    println!("supported : {}", p.supported().await.unwrap_or(false));
                    if let Ok(status) = p.status().await {
                        let mut keys: Vec<&String> = status.keys().collect();
                        keys.sort();
                        for k in keys {
                            if k == "active" || k == "supported" {
                                continue;
                            }
                            println!("{k:10}: {}", render_value(&status[k]));
                        }
                    }
                }
                None => {
                    let v = LocalView::detect();
                    let cfg = v.active_profile().game_config();
                    let topo = rimed_core::CoreTopology::detect_from(Path::new("/sys"));
                    println!("rimed not running — showing the local view.");
                    println!("supported : {}", cfg.enabled);
                    println!("tier      : {}", cfg.tier);
                    println!("cpuset    : {}", cfg.cpuset);
                    println!("irq       : {}", cfg.irq);
                    println!("cgroup    : {}", cfg.cgroup);
                    println!(
                        "cores     : P={} E={} (detected via {})",
                        if topo.pcore_list().is_empty() { "(none)".into() } else { topo.pcore_list() },
                        if topo.ecore_list().is_empty() { "(none)".into() } else { topo.ecore_list() },
                        topo.source.as_str()
                    );
                    // The STATE, not merely whether the file exists. On a
                    // machine that ships nvidia-smi with no driver loaded —
                    // the L16, measured — "present" is true and tells somebody
                    // debugging absent clock locks nothing at all.
                    println!("nvidia-smi: {}", rimed_core::gpu::nvidia_smi_state().as_str());
                    // Same rule, one tier over. Without this the degraded view
                    // said nothing at all about sched-ext, so the one surface
                    // a user reaches when the daemon is down was the one that
                    // could not tell them their kernel refuses every
                    // scheduler. `scx_state` needs the daemon; this does not.
                    let btf = rimed_core::kernelbtf::scx_btf_support(Path::new("/sys"));
                    // Resolved against this CPU: `auto` is scx_lavd on one
                    // kind of core and the kernel's scheduler on a P/E hybrid.
                    println!(
                        "scx       : {} (profile: {:?})",
                        cfg.scx_for(&topo)
                            .unwrap_or_else(|| "none, the kernel's own scheduler".into()),
                        cfg.scx.trim()
                    );
                    println!("scx_btf   : {}", btf.verdict());
                    if btf.blocks_loading() {
                        println!("            {}", btf.describe());
                    }
                }
            }
            0
        }
        GameCmd::Start { pid, owner_pid } => match &proxy {
            Some(p) => {
                // The owner is the atomic part: entering game mode and naming
                // the process that ends it must not be two calls with a window
                // between them in which the machine is tuned and unwatched.
                // A `--pid` given alongside is attached afterwards, because
                // that is a cpuset question and not a lifetime one.
                let res = match (owner_pid, pid) {
                    (Some(owner), _) => match p.start_owned_by(owner).await {
                        Ok(()) => match pid {
                            Some(pid) => p.attach_pid(pid).await,
                            None => Ok(()),
                        },
                        Err(e) => Err(e),
                    },
                    (None, Some(pid)) => p.start_for_pid(pid).await,
                    (None, None) => p.set_active(true).await,
                };
                match res {
                    Ok(()) => {
                        println!("rime: game mode ON");
                        0
                    }
                    Err(e) => {
                        eprintln!("rime: entering game mode failed: {e}");
                        1
                    }
                }
            }
            None => {
                eprintln!("rime: rimed not running — cannot enter game mode.");
                1
            }
        },
        GameCmd::Stop => match &proxy {
            Some(p) => match p.set_active(false).await {
                Ok(()) => {
                    println!("rime: game mode OFF");
                    0
                }
                Err(e) => {
                    eprintln!("rime: leaving game mode failed: {e}");
                    1
                }
            },
            None => {
                eprintln!("rime: rimed not running — cannot leave game mode.");
                1
            }
        },
        // Normally unreachable: the dispatch in `main` routes `profile` here
        // BEFORE this function, because everything above has already connected
        // to the system bus and `rime game profile apply`'s guard is only
        // meaningful when it is reached first.
        //
        // Routed rather than panicked on, because this crate's contract is a
        // clear message and a non-zero exit, never a panic — and routed rather
        // than wildcarded, so adding a verb to `GameCmd` is still a compile
        // error here. Reaching it costs a wasted bus connection and nothing
        // else: a connection raises no polkit prompt, and no mutating method
        // has been called at this point.
        GameCmd::Profile { cmd } => gaming::profile_main(cmd).await,
        GameCmd::Attach { pid } => match &proxy {
            Some(p) => match p.attach_pid(pid).await {
                Ok(()) => {
                    println!("rime: pid {pid} attached to the game cpuset");
                    0
                }
                Err(e) => {
                    eprintln!("rime: AttachPid failed: {e}");
                    1
                }
            },
            None => {
                eprintln!("rime: rimed not running — cannot attach a pid.");
                1
            }
        },
    }
}

/// Where the shell is vendored inside the image.
const SHELL_DIR_DEFAULT: &str = "/usr/share/rime-shell";

/// The shell config directory to address over IPC.
///
/// `RIME_SHELL_DIR` overrides it, matching the convention
/// /usr/libexec/rime-shell-autostart already uses. That is what makes it
/// possible to drive a working-tree checkout during development instead of only
/// the copy baked into the image. `APEX_SHELL_DIR`, the variable's name before
/// the rename, is read when the new one is unset, so a session environment or
/// a unit that still sets it keeps addressing the shell it launched.
fn shell_dir() -> String {
    ["RIME_SHELL_DIR", "APEX_SHELL_DIR"]  // rime-rename: keep (the override's name before the rename)
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|s| !s.trim().is_empty())
        .unwrap_or_else(|| SHELL_DIR_DEFAULT.to_string())
}

/// The mapping from `rime shell <verb>` to the shell's IPC surface.
///
/// Verb names are deliberately the user's vocabulary rather than the shell's
/// internal target strings: "settings" rather than "nexus", "power" rather than
/// "PowerMenu-toggle". That indirection is the point of the wrapper — the IPC
/// names can change without every keybind on every machine breaking.
fn shell_targets() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("launcher", "dashboard-launcher", "toggle"),
        ("dashboard", "dashboard-home", "toggle"),
        ("settings", "nexus", "toggle"),
        ("lock", "lockscreen", "lock"),
        ("notifications", "notification-toggle", "toggle"),
        ("clipboard", "clipboard-toggle", "toggle"),
        ("wallpaper", "wallpaper-toggle", "toggle"),
        ("menu", "context-menu", "toggle"),
        ("power", "PowerMenu-toggle", "toggle"),
        ("audio out", "audioOut-toggle", "toggle"),
        ("audio in", "audioIn-toggle", "toggle"),
        ("audio mixer", "audioMix-toggle", "toggle"),
        ("network wifi", "wifi-toggle", "toggle"),
        ("network bluetooth", "bluetooth-toggle", "toggle"),
        ("network vpn", "vpn-toggle", "toggle"),
        ("network hotspot", "hotspot-toggle", "toggle"),
        ("focus", "focus-toggle", "toggle"),
        ("record", "screenrec-on", "toggle"),
        ("voice", "voice-ptt", "toggle"),
        ("switcher next", "window-switcher", "next"),
        ("switcher prev", "window-switcher", "prev"),
        ("switcher commit", "window-switcher", "commit"),
        ("switcher cancel", "window-switcher", "cancel"),
    ]
}

/// Why an IPC call did not succeed.
///
/// Distinguished rather than collapsed into one error because they call for
/// completely different responses: "you are not in a graphical session", "your
/// shell predates this CLI" and "the shell is not running" have nothing to do
/// with each other.
#[derive(Debug, PartialEq, Eq)]
enum IpcFailure {
    /// `qs` is not installed — not a graphical session.
    QsMissing,
    /// No shell config at the addressed path.
    MissingConfig,
    /// The shell answered, but exposes no such target (or function).
    MissingHandler { function: bool },
    /// No shell instance is running.
    NotRunning,
    /// Anything else, carrying whatever the tool said.
    Other(String),
}

/// Classify a completed `qs ipc call`.
///
/// Shared by both callers, deliberately. `qs ipc call` exits ZERO for "Target
/// not found.", "Function not found." and "Could not open config file" — it only
/// fails properly (255) when no instance is running. Trusting the exit status
/// reports success for a call that did nothing, which from a keybind is
/// indistinguishable from a dead key.
///
/// Applying this in only ONE of the two callers is exactly the bug this function
/// exists to prevent: the query path previously treated "Target not found." as a
/// successful result and printed it as data, so `settings --list` against an
/// older shell listed "Target", "not" and "found." as pages and exited 0.
fn classify_qs(code: i32, stdout: &str, stderr: &str) -> Option<IpcFailure> {
    let combined = format!("{stdout}{stderr}");

    if combined.contains("Could not open config file") {
        return Some(IpcFailure::MissingConfig);
    }
    if combined.contains("Target not found") {
        return Some(IpcFailure::MissingHandler { function: false });
    }
    if combined.contains("Function not found") {
        return Some(IpcFailure::MissingHandler { function: true });
    }
    if combined.contains("No running instances") {
        return Some(IpcFailure::NotRunning);
    }
    if code != 0 {
        return Some(IpcFailure::Other(combined));
    }
    None
}

/// Run one IPC call, returning `(stdout, stderr)` on success.
///
/// `qs` is Quickshell's own CLI and is what actually speaks the protocol; there
/// is no D-Bus route to the shell to use instead.
fn qs_call(target: &str, function: &str, args: &[String]) -> Result<(String, String), IpcFailure> {
    use std::process::Command;

    let mut argv: Vec<String> = vec![
        "-p".into(),
        shell_dir(),
        "ipc".into(),
        "call".into(),
        target.into(),
        function.into(),
    ];
    argv.extend(args.iter().cloned());

    let out = match Command::new("qs").args(&argv).output() {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(IpcFailure::QsMissing),
        Err(e) => return Err(IpcFailure::Other(format!("could not run qs: {e}"))),
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    match classify_qs(out.status.code().unwrap_or(1), &stdout, &stderr) {
        Some(f) => Err(f),
        None => Ok((stdout, stderr)),
    }
}

fn report_ipc_failure(f: &IpcFailure, target: &str, function: &str) {
    match f {
        IpcFailure::QsMissing => eprintln!(
            "rime: `qs` (Quickshell) not found. `rime shell` drives the running \
             shell over its IPC, so it only works inside a graphical session."
        ),
        IpcFailure::MissingConfig => eprintln!(
            "rime: no shell config at {}. Set RIME_SHELL_DIR to point at a \
             checkout, or reinstall the image copy.",
            shell_dir()
        ),
        IpcFailure::MissingHandler { function: is_fn } => {
            let what = if *is_fn { "function" } else { "target" };
            eprintln!(
                "rime: the running Rime Shell does not expose {what} \
                 '{target} {function}'.\n\
                 This usually means the shell is older than this CLI — \
                 `rime update` and log back in.\n\
                 `rime shell list` shows what this wrapper knows about."
            );
        }
        IpcFailure::NotRunning => eprintln!(
            "rime: Rime Shell is not running (addressing {}).\n\
             Start or repair it with: /usr/libexec/rime-shell-autostart",
            shell_dir()
        ),
        IpcFailure::Other(msg) => {
            eprintln!("rime: shell IPC '{target} {function}' failed.");
            if !msg.trim().is_empty() {
                eprint!("{msg}");
            }
        }
    }
}

/// Fire and forget: forward whatever the handler returned.
fn shell_ipc(target: &str, function: &str, args: &[String]) -> i32 {
    match qs_call(target, function, args) {
        Ok((stdout, stderr)) => {
            // Handlers return strings ("nexus open at appearance"); pass them
            // through so scripting can read them.
            if !stdout.trim().is_empty() {
                print!("{stdout}");
            }
            if !stderr.trim().is_empty() {
                eprint!("{stderr}");
            }
            0
        }
        Err(f) => {
            report_ipc_failure(&f, target, function);
            1
        }
    }
}

/// Capture a handler's return value, for the queries.
///
/// Uses the same classification as `shell_ipc`, so a failure can never be
/// mistaken for data.
fn shell_ipc_query(target: &str, function: &str) -> Result<String, IpcFailure> {
    qs_call(target, function, &[]).map(|(stdout, _)| stdout.trim().to_string())
}

fn cmd_shell(cmd: ShellCmd) -> i32 {
    match cmd {
        ShellCmd::Launcher => shell_ipc("dashboard-launcher", "toggle", &[]),

        ShellCmd::Dashboard { page } => {
            // The dashboard exposes one target per page rather than a target
            // taking an argument, so the page becomes part of the target name.
            let page = page.unwrap_or_else(|| "home".into());
            const PAGES: [&str; 5] = ["home", "stats", "kanban", "launcher", "config"];
            if !PAGES.contains(&page.as_str()) {
                eprintln!(
                    "rime: unknown dashboard page '{page}' (try: {})",
                    PAGES.join(", ")
                );
                return 1;
            }
            shell_ipc(&format!("dashboard-{page}"), "toggle", &[])
        }

        ShellCmd::Settings { page, list, close } => {
            if list {
                // Ask the shell rather than hardcoding: the page set lives in
                // the shell's PageRegistry and this must not drift from it.
                return match shell_ipc_query("nexus", "pages") {
                    Ok(s) if !s.is_empty() => {
                        for p in s.split_whitespace() {
                            println!("{p}");
                        }
                        0
                    }
                    Ok(_) => {
                        eprintln!("rime: the shell returned no settings pages.");
                        1
                    }
                    Err(f) => {
                        report_ipc_failure(&f, "nexus", "pages");
                        1
                    }
                };
            }
            if close {
                return shell_ipc("nexus", "close", &[]);
            }
            match page {
                Some(p) => shell_ipc("nexus", "toggle", &[p]),
                None => shell_ipc("nexus", "toggle", &[]),
            }
        }

        ShellCmd::Lock => shell_ipc("lockscreen", "lock", &[]),
        ShellCmd::Notifications => shell_ipc("notification-toggle", "toggle", &[]),
        ShellCmd::Clipboard => shell_ipc("clipboard-toggle", "toggle", &[]),
        ShellCmd::Wallpaper => shell_ipc("wallpaper-toggle", "toggle", &[]),
        ShellCmd::Menu => shell_ipc("context-menu", "toggle", &[]),
        ShellCmd::Power => shell_ipc("PowerMenu-toggle", "toggle", &[]),
        ShellCmd::Focus => shell_ipc("focus-toggle", "toggle", &[]),
        ShellCmd::Record => shell_ipc("screenrec-on", "toggle", &[]),
        ShellCmd::Voice => shell_ipc("voice-ptt", "toggle", &[]),

        // Four functions on one target rather than four targets, because they
        // are four operations on one piece of state and the shell has to see
        // them arrive in order.
        ShellCmd::Switcher { action } => {
            let func = match action.as_str() {
                "next" | "prev" | "commit" | "cancel" => action.as_str(),
                other => {
                    eprintln!(
                        "rime: unknown switcher action '{other}' \
                         (try: next, prev, commit, cancel)"
                    );
                    return 1;
                }
            };
            shell_ipc("window-switcher", func, &[])
        }

        ShellCmd::Audio { which } => {
            let target = match which.as_str() {
                "out" | "output" | "sink" => "audioOut-toggle",
                "in" | "input" | "source" | "mic" => "audioIn-toggle",
                "mixer" | "mix" | "apps" => "audioMix-toggle",
                other => {
                    eprintln!("rime: unknown audio panel '{other}' (try: out, in, mixer)");
                    return 1;
                }
            };
            shell_ipc(target, "toggle", &[])
        }

        ShellCmd::Network { tab } => {
            let target = match tab.as_str() {
                "wifi" | "wlan" => "wifi-toggle",
                "bluetooth" | "bt" => "bluetooth-toggle",
                "vpn" => "vpn-toggle",
                "hotspot" | "ap" => "hotspot-toggle",
                other => {
                    eprintln!(
                        "rime: unknown network tab '{other}' \
                         (try: wifi, bluetooth, vpn, hotspot)"
                    );
                    return 1;
                }
            };
            shell_ipc(target, "toggle", &[])
        }

        ShellCmd::List => {
            let rows = shell_targets();
            let width = rows.iter().map(|(v, ..)| v.len()).max().unwrap_or(0);
            println!("{:<width$}  IPC CALL", "rime shell …", width = width);
            for (verb, target, func) in rows {
                println!("{verb:<width$}  {target} {func}", width = width);
            }
            println!();
            println!("Anything else: rime shell ipc <target> <function> [args…]");
            0
        }

        ShellCmd::Ipc {
            target,
            function,
            args,
        } => shell_ipc(&target, &function, &args),
    }
}

/// `rime metrics` — read rimed's telemetry snapshot.
///
/// The data already existed in two places rimed exposes: the
/// `org.rimeos.Rimed1.Metrics.Snapshot` property and the Prometheus endpoint on
/// 127.0.0.1:9723. Neither was reachable from the CLI, so checking package power
/// or a thermal zone meant hand-writing a `busctl get-property` invocation or
/// curling a port. This is purely additive to the frozen D-Bus contract: it adds
/// a proxy and a verb, and changes nothing daemon-side.
///
/// Read-only, so deliberately absent from the privileged-command match: it must
/// stay usable without root.
async fn cmd_metrics(args: MetricsArgs) -> i32 {
    let Some(conn) = connect().await else {
        eprintln!("rime: cannot reach the system bus.");
        return 1;
    };

    if !daemon_running(&conn).await {
        eprintln!("rime: rimed not running — no metrics to read.");
        return 1;
    }

    let proxy = match MetricsProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rime: cannot reach the Metrics interface: {e}");
            return 1;
        }
    };

    // Clamp the interval: a zero or negative period would spin the daemon.
    let interval = args
        .stream
        .map(|s| Duration::from_secs_f64(if s.is_finite() && s >= 0.1 { s } else { 0.1 }));

    loop {
        match proxy.snapshot().await {
            Ok(snap) => {
                if args.json {
                    println!("{}", snapshot_to_json(&snap));
                } else {
                    print_snapshot_table(&snap);
                }
            }
            Err(e) => {
                eprintln!("rime: reading the snapshot failed: {e}");
                // A one-shot read reports the failure; a stream keeps trying, so
                // a daemon restart does not end a long-running collector.
                if interval.is_none() {
                    return 1;
                }
            }
        }

        match interval {
            Some(d) => {
                // Without this a piped consumer sees nothing until the pipe
                // buffer fills, which for one small sample per interval can be
                // minutes.
                use std::io::Write;
                let _ = std::io::stdout().flush();
                tokio::time::sleep(d).await;
            }
            None => return 0,
        }
    }
}

/// Stable, human-sensible key order: the headline fields first in a fixed order,
/// then everything else (the `temp_<zone>` set, whose membership is per-machine)
/// alphabetically so successive samples line up.
fn snapshot_key_order(snap: &std::collections::HashMap<String, zvariant::OwnedValue>) -> Vec<String> {
    const PREFERRED: [&str; 4] = ["tier", "on_ac", "ppt_watts", "battery_uwh"];

    let mut out: Vec<String> = PREFERRED
        .iter()
        .filter(|k| snap.contains_key(**k))
        .map(|k| (*k).to_string())
        .collect();

    let mut rest: Vec<String> = snap
        .keys()
        .filter(|k| !PREFERRED.contains(&k.as_str()))
        .cloned()
        .collect();
    rest.sort();
    out.extend(rest);
    out
}

fn print_snapshot_table(snap: &std::collections::HashMap<String, zvariant::OwnedValue>) {
    let keys = snapshot_key_order(snap);
    let width = keys.iter().map(|k| k.len()).max().unwrap_or(0);
    for k in keys {
        if let Some(v) = snap.get(&k) {
            println!("{:<width$}  {}", k, render_value(v), width = width);
        }
    }
}

/// Minimal JSON encoder for the snapshot.
///
/// Hand-rolled rather than pulling serde_json in: `rime` ships in a signed image
/// and this is the only place in the CLI that needs JSON, so a few lines of
/// escaping is a better trade than another dependency in the tree.
fn snapshot_to_json(snap: &std::collections::HashMap<String, zvariant::OwnedValue>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for k in snapshot_key_order(snap) {
        if let Some(v) = snap.get(&k) {
            parts.push(format!("{}:{}", json_string(&k), json_value(v)));
        }
    }
    format!("{{{}}}", parts.join(","))
}

fn json_value(v: &zvariant::OwnedValue) -> String {
    fn inner(v: &zvariant::Value<'_>) -> String {
        use zvariant::Value;
        match v {
            Value::Str(s) => json_string(s.as_str()),
            Value::Bool(b) => b.to_string(),
            Value::U8(n) => n.to_string(),
            Value::U16(n) => n.to_string(),
            Value::U32(n) => n.to_string(),
            Value::U64(n) => n.to_string(),
            Value::I16(n) => n.to_string(),
            Value::I32(n) => n.to_string(),
            Value::I64(n) => n.to_string(),
            // Non-finite floats have no JSON representation; null is the only
            // honest answer and parsers accept it.
            Value::F64(n) => {
                if n.is_finite() {
                    format!("{n}")
                } else {
                    "null".to_string()
                }
            }
            Value::Array(a) => format!(
                "[{}]",
                a.iter().map(inner).collect::<Vec<_>>().join(",")
            ),
            Value::Value(b) => inner(b),
            other => json_string(&format!("{other:?}")),
        }
    }
    inner(v)
}

pub(crate) fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // JSON requires escaping everything below 0x20.
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

async fn cmd_doctor(json: bool) -> i32 {
    let v = LocalView::detect();
    let conn = connect().await;
    let running = match &conn {
        Some(c) => daemon_running(c).await,
        None => false,
    };

    // The checks themselves live in `recover`, so §19's graphical surface and
    // this command are the same list rendered twice rather than two lists that
    // can disagree. Everything except the metrics probe is a file read, and
    // the probe stays here because it is the one check that needs a socket.
    let mut checks = recover::doctor_checks(&v, running);
    // A refused connection and a connection nobody could attempt are different
    // facts about this machine, and `.is_ok()` returned false for both — so
    // the line a person read was the same sentence either way. That is this
    // repository's "permission denied is not absence", one layer down: a
    // machine whose networking is gone was told its metrics endpoint is not
    // reachable, which is a claim about rimed made out of a syscall that never
    // left the box.
    //
    // The boolean stays, and both remain a WARN, because neither state is a
    // machine whose endpoint is reachable — `Check` has no third arm and
    // inventing one here would be a judgement the other checks do not make
    // (see `render_doctor`'s note on severity). What changes is the sentence,
    // which is the part somebody acts on.
    //
    // tests/chaos/cases/network-loss.sh holds this, by running the same verb
    // in three real network namespaces — loopback down, loopback up with
    // nothing listening, and a listener bound — and asserting the first two do
    // not read identically.
    let metrics = TcpStream::connect_timeout(
        &"127.0.0.1:9723".parse::<SocketAddr>().unwrap(),
        Duration::from_millis(200),
    );
    let (metrics_up, metrics_what) = match &metrics {
        Ok(_) => (true, "metrics endpoint reachable on 127.0.0.1:9723".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => (
            false,
            "metrics endpoint on 127.0.0.1:9723 refused the connection — nothing \
             is listening there, so rimed is not serving metrics"
                .to_string(),
        ),
        Err(e) => (
            false,
            format!(
                "metrics endpoint on 127.0.0.1:9723 could not be probed at all \
                 ({e}) — this machine's networking did not answer, so whether \
                 rimed is serving metrics is unknown"
            ),
        ),
    };
    checks.push(recover::Check {
        ok: metrics_up,
        what: metrics_what,
    });

    // §48: disk-health warnings reach the doctor. Only the rows with something
    // to do become a WARN — a row nobody could measure is printed with its
    // reason and passes, because `rime doctor` runs unprivileged and a SMART
    // log nobody could open must not turn every run red.
    for (ok, what) in storage::doctor_lines(&storage::Roots::from_env()) {
        checks.push(recover::Check { ok, what });
    }

    // §P2-015: the same rule for firmware. An update waiting is a WARN; a
    // machine where fwupd could not be consulted at all passes with the
    // reason on the line, because `rime doctor` runs unprivileged and
    // measured, nothing in any Containerfile installs fwupd today.
    for (ok, what) in firmware::doctor_lines(&firmware::Roots::from_env()) {
        checks.push(recover::Check { ok, what });
    }

    // An NVIDIA GPU older than the driver branch in the image (Maxwell,
    // Pascal, Volta and earlier) gets no driver at all on Rime, and nothing
    // else on the machine says so. A machine with no NVIDIA GPU gets no line.
    if let Some((ok, what)) =
        rimed_core::nvidia_support::scan(std::path::Path::new("/sys")).doctor_line()
    {
        checks.push(recover::Check { ok, what });
    }

    print!("{}", recover::render_doctor(&checks, json));
    0
}

// ── small helpers ────────────────────────────────────────────────────────────

/// Render one `a{sv}` fan entry as a single line.
fn render_fan(f: &std::collections::HashMap<String, zvariant::OwnedValue>) -> String {
    let get = |k: &str| f.get(k).map(render_value);
    let mut parts = vec![get("id").unwrap_or_else(|| "?".into())];
    if let Some(rpm) = get("rpm") {
        parts.push(format!("{rpm} rpm"));
    }
    if let Some(pct) = get("percent") {
        parts.push(format!("{pct}%"));
    }
    if let Some(pwm) = get("pwm") {
        parts.push(format!("pwm {pwm}"));
    }
    if get("controllable").as_deref() == Some("true") {
        parts.push("controllable".into());
    }
    parts.join("  ")
}

/// Human rendering for the handful of D-Bus variant types rimed returns.
fn render_value(v: &zvariant::OwnedValue) -> String {
    fn inner(v: &zvariant::Value<'_>) -> String {
        use zvariant::Value;
        match v {
            Value::Str(s) => s.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::U8(n) => n.to_string(),
            Value::U16(n) => n.to_string(),
            Value::U32(n) => n.to_string(),
            Value::U64(n) => n.to_string(),
            Value::I16(n) => n.to_string(),
            Value::I32(n) => n.to_string(),
            Value::I64(n) => n.to_string(),
            Value::F64(n) => format!("{n:.2}"),
            Value::Array(a) => a.iter().map(inner).collect::<Vec<_>>().join(", "),
            Value::Value(b) => inner(b),
            other => format!("{other:?}"),
        }
    }
    inner(v)
}

fn print_kv(key: &str, val: Option<String>) {
    if let Some(v) = val {
        println!("{key}: {v}");
    }
}

/// `pub(crate)` because the doctor's checks moved to `recover`, where §19's
/// JSON rendering of them lives. The reader stays here rather than being
/// duplicated: two `/sys` readers with different trimming rules would answer
/// the same question two ways.
pub(crate) fn read_sys(rel: &str) -> Option<String> {
    read_abs(&format!("/sys/{rel}"))
}

fn read_abs(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

// ── Tests ────────────────────────────────────────────────────────────────────
// `rime install` hands its arguments to a separate process, so nothing here is
// type-checked against the engine. These pin the two things that would fail
// silently: that a path is accepted where a package name goes, and that the
// unverified-RPM opt-in is off unless asked for and reaches the engine when it
// is asked for.
#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// The parsed pieces of an `rime install`, in the order `install_argv`
    /// takes them.
    struct Install {
        packages: Vec<String>,
        no_weak_deps: bool,
        enable_repo: Vec<String>,
        allow_unsigned: bool,
        source: Option<String>,
        env: Option<String>,
    }

    fn install(argv: &[&str]) -> Install {
        match Cli::try_parse_from(argv).expect("parses").command {
            Cmd::Install {
                packages,
                no_weak_deps,
                enable_repo,
                allow_unsigned,
                source,
                env,
            } => Install {
                packages,
                no_weak_deps,
                enable_repo,
                allow_unsigned,
                source,
                env,
            },
            _ => panic!("not an install"),
        }
    }

    /// The engine argv an `rime install` command line produces.
    fn install_engine_argv(argv: &[&str]) -> Vec<String> {
        let i = install(argv);
        install_argv(
            i.packages,
            i.no_weak_deps,
            i.enable_repo,
            i.allow_unsigned,
            i.source,
            i.env,
        )
    }

    #[test]
    fn the_cli_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn only_the_channel_verb_that_writes_needs_root() {
        // The whole point of the readout is that it works when the machine is
        // in trouble. A password prompt in front of "which channel am I on"
        // would be exactly the wrong time to ask.
        for argv in [
            vec!["rime", "channel", "status"],
            vec!["rime", "channel", "list"],
            vec!["rime", "channel", "report"],
        ] {
            let cli = Cli::try_parse_from(&argv).expect("parses");
            assert_eq!(privileged_verb(&cli.command), None, "{argv:?}");
        }
        let cli = Cli::try_parse_from(["rime", "channel", "set", "stable"]).expect("parses");
        assert_eq!(privileged_verb(&cli.command), Some("channel set"));
        // A name nobody has heard of is refused by clap, BEFORE the root gate.
        // It was the other way round, and the user was told to type sudo and
        // then told they had made a typo.
        let e = match Cli::try_parse_from(["rime", "channel", "set", "nightly"]) {
            Ok(_) => panic!("an invented channel must not parse"),
            Err(e) => e.to_string(),
        };
        for name in ["stable", "candidate", "beta", "edge"] {
            assert!(e.contains(name), "the refusal must name {name}: {e}");
        }
        assert!(!e.contains("root"), "the refusal must not ask for a password: {e}");
        // And a dry run still needs it: it is the same verb, and classifying
        // by flag is how a refusal ends up depending on argument order.
        let cli =
            Cli::try_parse_from(["rime", "channel", "set", "stable", "--dry-run"]).expect("parses");
        assert_eq!(privileged_verb(&cli.command), Some("channel set"));
    }

    #[test]
    fn shell_is_not_a_privileged_verb() {
        // `rime shell` drives the user's own session over IPC. Requiring root
        // would be both wrong and useless: root has no WAYLAND_DISPLAY, so the
        // call could not reach the shell anyway.
        let cli = Cli::try_parse_from(["rime", "shell", "launcher"]).expect("parses");
        assert!(
            !matches!(cli.command, Cmd::Update(_) | Cmd::Pin | Cmd::Rollback),
            "shell must not be classified with the root-only verbs"
        );
    }

    fn shell_cmd(argv: &[&str]) -> ShellCmd {
        match Cli::try_parse_from(argv).expect("parses").command {
            Cmd::Shell { cmd } => cmd,
            _ => panic!("not a shell command"),
        }
    }

    #[test]
    fn shell_verbs_parse() {
        assert!(matches!(shell_cmd(&["rime", "shell", "launcher"]), ShellCmd::Launcher));
        assert!(matches!(shell_cmd(&["rime", "shell", "lock"]), ShellCmd::Lock));
        assert!(matches!(shell_cmd(&["rime", "shell", "list"]), ShellCmd::List));
    }

    #[test]
    fn dashboard_page_is_optional() {
        match shell_cmd(&["rime", "shell", "dashboard"]) {
            ShellCmd::Dashboard { page } => assert_eq!(page, None),
            _ => panic!("wrong variant"),
        }
        match shell_cmd(&["rime", "shell", "dashboard", "stats"]) {
            ShellCmd::Dashboard { page } => assert_eq!(page.as_deref(), Some("stats")),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn settings_takes_a_page_or_a_query_or_a_close() {
        // A bare `rime shell settings` must work as a single keybind.
        match shell_cmd(&["rime", "shell", "settings"]) {
            ShellCmd::Settings { page, list, close } => {
                assert_eq!(page, None);
                assert!(!list);
                assert!(!close);
            }
            _ => panic!("wrong variant"),
        }
        match shell_cmd(&["rime", "shell", "settings", "keybinds"]) {
            ShellCmd::Settings { page, .. } => assert_eq!(page.as_deref(), Some("keybinds")),
            _ => panic!("wrong variant"),
        }
        assert!(matches!(
            shell_cmd(&["rime", "shell", "settings", "--list"]),
            ShellCmd::Settings { list: true, .. }
        ));
        assert!(matches!(
            shell_cmd(&["rime", "shell", "settings", "--close"]),
            ShellCmd::Settings { close: true, .. }
        ));
    }

    #[test]
    fn contradictory_settings_flags_are_rejected_not_guessed() {
        // Silently letting one win is how a script ends up doing the opposite of
        // what it reads as.
        for argv in [
            vec!["rime", "shell", "settings", "--list", "--close"],
            vec!["rime", "shell", "settings", "keybinds", "--list"],
            vec!["rime", "shell", "settings", "keybinds", "--close"],
        ] {
            assert!(
                Cli::try_parse_from(&argv).is_err(),
                "{argv:?} should have been rejected"
            );
        }
    }

    #[test]
    fn qs_silent_failures_are_classified_despite_a_zero_exit() {
        // The whole point: `qs ipc call` exits 0 for these, so a caller trusting
        // the exit status treats a call that did nothing as a success. The query
        // path once printed "Target not found." as if it were page data.
        assert_eq!(
            classify_qs(0, "Target not found.\n", ""),
            Some(IpcFailure::MissingHandler { function: false })
        );
        assert_eq!(
            classify_qs(0, "Function not found.\n", ""),
            Some(IpcFailure::MissingHandler { function: true })
        );
        assert_eq!(
            classify_qs(0, "Could not open config file at \"/nope\"\n", ""),
            Some(IpcFailure::MissingConfig)
        );
        // This one does exit non-zero (255), but must still be named rather
        // than lumped into Other.
        assert_eq!(
            classify_qs(255, "No running instances for \"/x/shell.qml\"\n", ""),
            Some(IpcFailure::NotRunning)
        );
    }

    #[test]
    fn a_real_handler_reply_is_not_mistaken_for_a_failure() {
        assert_eq!(classify_qs(0, "nexus open at keybinds\n", ""), None);
        assert_eq!(classify_qs(0, "appearance layout data keybinds misc\n", ""), None);
        // Empty output with a clean exit is a valid void handler.
        assert_eq!(classify_qs(0, "", ""), None);
    }

    #[test]
    fn an_unexplained_nonzero_exit_is_still_a_failure() {
        match classify_qs(3, "", "something went wrong") {
            Some(IpcFailure::Other(msg)) => assert!(msg.contains("something went wrong")),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn audio_and_network_default_to_their_common_case() {
        match shell_cmd(&["rime", "shell", "audio"]) {
            ShellCmd::Audio { which } => assert_eq!(which, "out"),
            _ => panic!("wrong variant"),
        }
        match shell_cmd(&["rime", "shell", "network"]) {
            ShellCmd::Network { tab } => assert_eq!(tab, "wifi"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ipc_passes_arguments_through_verbatim() {
        // The escape hatch must not filter or reorder: it exists precisely for
        // handlers this wrapper does not know about.
        match shell_cmd(&["rime", "shell", "ipc", "nexus", "open", "keybinds", "extra"]) {
            ShellCmd::Ipc {
                target,
                function,
                args,
            } => {
                assert_eq!(target, "nexus");
                assert_eq!(function, "open");
                assert_eq!(args, vec!["keybinds".to_string(), "extra".to_string()]);
            }
            _ => panic!("wrong variant"),
        }
        // Function defaults to toggle, which is what most handlers expose.
        match shell_cmd(&["rime", "shell", "ipc", "focus-toggle"]) {
            ShellCmd::Ipc { function, .. } => assert_eq!(function, "toggle"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn the_target_table_is_self_consistent() {
        let rows = shell_targets();
        assert!(!rows.is_empty());
        let mut seen = std::collections::HashSet::new();
        for (verb, target, func) in &rows {
            assert!(!verb.is_empty() && !target.is_empty() && !func.is_empty());
            assert!(seen.insert(*verb), "duplicate verb in the table: {verb}");
        }
        // `rime shell list` is documentation, so it must actually cover the
        // verbs that exist rather than drifting from them.
        for expect in [
            "launcher",
            "settings",
            "lock",
            "power",
            "focus",
            "record",
            "switcher next",
            "switcher commit",
        ] {
            assert!(
                rows.iter().any(|(v, ..)| *v == expect),
                "{expect} missing from the target table"
            );
        }
    }

    #[test]
    fn switcher_actions_parse_and_are_bounded() {
        match shell_cmd(&["rime", "shell", "switcher", "next"]) {
            ShellCmd::Switcher { action } => assert_eq!(action, "next"),
            _ => panic!("not switcher"),
        }
        match shell_cmd(&["rime", "shell", "switcher", "commit"]) {
            ShellCmd::Switcher { action } => assert_eq!(action, "commit"),
            _ => panic!("not switcher"),
        }
        // The action is a free string at the clap layer, so the rejection of a
        // wrong one lives in the dispatch arm. What is asserted here is that
        // every action the compositor configs actually bind is in the table —
        // a verb the keybinds use and the CLI rejects is a dead shortcut.
        let rows = shell_targets();
        for action in ["next", "prev", "commit", "cancel"] {
            let verb = format!("switcher {action}");
            let row = rows.iter().find(|(v, ..)| *v == verb);
            let (_, target, func) = row.unwrap_or_else(|| panic!("{verb} missing"));
            assert_eq!(*target, "window-switcher");
            assert_eq!(*func, action);
        }
    }

    #[test]
    fn shell_dir_is_overridable_for_development() {
        // Not asserting the env var here (tests share a process); asserting the
        // default, which is the contract keybinds rely on.
        assert_eq!(SHELL_DIR_DEFAULT, "/usr/share/rime-shell");
    }

    fn metrics(argv: &[&str]) -> MetricsArgs {
        match Cli::try_parse_from(argv).expect("parses").command {
            Cmd::Metrics(a) => a,
            _ => panic!("not metrics"),
        }
    }

    #[test]
    fn metrics_defaults_to_a_single_human_readable_sample() {
        let a = metrics(&["rime", "metrics"]);
        assert!(!a.json);
        assert!(a.stream.is_none(), "must not stream unless asked");
    }

    #[test]
    fn metrics_stream_has_a_default_interval_but_takes_one() {
        // Bare --stream is the common case and must not require a number.
        assert_eq!(metrics(&["rime", "metrics", "--stream"]).stream, Some(2.0));
        assert_eq!(
            metrics(&["rime", "metrics", "--stream", "0.5"]).stream,
            Some(0.5)
        );
        assert!(metrics(&["rime", "metrics", "--json", "--stream", "1"]).json);
    }

    #[test]
    fn snapshot_keys_are_ordered_stably_for_diffing() {
        use std::collections::HashMap;
        use zvariant::Value;

        let mut m: HashMap<String, zvariant::OwnedValue> = HashMap::new();
        for k in [
            "temp_k10temp",
            "battery_uwh",
            "temp_acpitz",
            "on_ac",
            "tier",
            "ppt_watts",
        ] {
            m.insert(
                k.to_string(),
                zvariant::OwnedValue::try_from(Value::from(1u32)).unwrap(),
            );
        }

        // Headline fields in a fixed order, then the per-machine temp_* set
        // alphabetically so successive samples line up column-wise.
        assert_eq!(
            snapshot_key_order(&m),
            vec![
                "tier",
                "on_ac",
                "ppt_watts",
                "battery_uwh",
                "temp_acpitz",
                "temp_k10temp"
            ]
        );
    }

    #[test]
    fn snapshot_key_order_omits_fields_the_machine_cannot_report() {
        use std::collections::HashMap;
        use zvariant::Value;

        let mut m: HashMap<String, zvariant::OwnedValue> = HashMap::new();
        m.insert(
            "tier".to_string(),
            zvariant::OwnedValue::try_from(Value::from("balanced")).unwrap(),
        );
        // A desktop reports no battery and no ppt; those keys must simply be
        // absent rather than rendered empty.
        assert_eq!(snapshot_key_order(&m), vec!["tier"]);
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_string("a\nb"), "\"a\\nb\"");
        // Control characters must be \u-escaped or the output is not JSON.
        assert_eq!(json_string("a\u{1}b"), "\"a\\u0001b\"");
    }

    #[test]
    fn json_snapshot_is_well_formed_and_typed() {
        use std::collections::HashMap;
        use zvariant::Value;

        let mut m: HashMap<String, zvariant::OwnedValue> = HashMap::new();
        m.insert(
            "tier".to_string(),
            zvariant::OwnedValue::try_from(Value::from("ultra")).unwrap(),
        );
        m.insert(
            "on_ac".to_string(),
            zvariant::OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        m.insert(
            "ppt_watts".to_string(),
            zvariant::OwnedValue::try_from(Value::from(15.5f64)).unwrap(),
        );

        let js = snapshot_to_json(&m);
        assert_eq!(js, r#"{"tier":"ultra","on_ac":true,"ppt_watts":15.5}"#);
    }

    #[test]
    fn non_finite_floats_become_null_not_invalid_json() {
        use std::collections::HashMap;
        use zvariant::Value;

        let mut m: HashMap<String, zvariant::OwnedValue> = HashMap::new();
        m.insert(
            "ppt_watts".to_string(),
            zvariant::OwnedValue::try_from(Value::from(f64::NAN)).unwrap(),
        );
        // NaN has no JSON representation; emitting a bare NaN would produce
        // output no parser accepts.
        assert_eq!(snapshot_to_json(&m), r#"{"ppt_watts":null}"#);
    }

    #[test]
    fn install_takes_a_local_rpm_path_as_a_package() {
        // The engine decides what is a file and what is a package name; the CLI
        // must not filter, reorder or reject either form.
        let i = install(&[
            "rime",
            "install",
            "/media/usb/google-chrome-stable.rpm",
            "htop",
            "org.gimp.GIMP",
        ]);
        assert_eq!(
            i.packages,
            vec![
                "/media/usb/google-chrome-stable.rpm".to_string(),
                "htop".to_string(),
                "org.gimp.GIMP".to_string(),
            ]
        );
    }

    #[test]
    fn a_path_with_spaces_survives_as_one_argument() {
        let i = install(&["rime", "install", "/media/My Stick/an app.rpm"]);
        assert_eq!(i.packages, vec!["/media/My Stick/an app.rpm".to_string()]);
    }

    #[test]
    fn the_unverified_opt_in_is_off_unless_asked_for() {
        assert!(!install(&["rime", "install", "./x.rpm"]).allow_unsigned);
        assert!(!install_engine_argv(&["rime", "install", "./x.rpm"])
            .contains(&"--allow-unsigned".to_string()));
    }

    #[test]
    fn every_flag_reaches_the_engine_argv() {
        assert_eq!(
            install_engine_argv(&[
                "rime",
                "install",
                "--allow-unsigned",
                "--no-weak-deps",
                "--enable-repo",
                "extra",
                "./x.rpm",
            ]),
            vec![
                "install".to_string(),
                "./x.rpm".to_string(),
                "--no-weak-deps".to_string(),
                "--enable-repo=extra".to_string(),
                "--allow-unsigned".to_string(),
            ]
        );
    }

    // ── the firewall helper's argv ──────────────────────────────────────────
    //
    // `firewall_argv`'s own doc comment says it is kept pure "so the argv this
    // hands a root-run helper can be asserted without running it". Nothing
    // asserted it. That is the shape of defect this program keeps finding — a
    // stated property with no test behind it — and it mattered the moment
    // `status` grew a flag, because `rime` is clap: a word this function does
    // not emit is a word the helper never sees, however well the helper
    // supports it.

    /// The helper argv an `rime firewall ...` command line produces, through
    /// clap, rather than by constructing the enum by hand — so a flag that
    /// clap would reject cannot pass here.
    fn firewall_engine_argv(argv: &[&str]) -> Vec<String> {
        match Cli::parse_from(argv).command {
            Cmd::Firewall { cmd } => firewall_argv(cmd),
            _ => panic!("not a firewall command"),
        }
    }

    #[test]
    fn status_is_prose_unless_json_is_asked_for() {
        assert_eq!(
            firewall_engine_argv(&["rime", "firewall", "status"]),
            vec!["status".to_string()]
        );
    }

    #[test]
    fn the_json_flag_reaches_the_helper() {
        assert_eq!(
            firewall_engine_argv(&["rime", "firewall", "status", "--json"]),
            vec!["status".to_string(), "--json".to_string()]
        );
    }

    #[test]
    fn every_firewall_verb_reaches_the_helper_by_its_own_name() {
        // The helper dispatches on this word. A verb renamed here and not
        // there is not a compile error — it is `rime firewall reload` exiting
        // 2 with the helper's usage, which reads like the user's mistake.
        assert_eq!(
            firewall_engine_argv(&["rime", "firewall", "list"]),
            vec!["list".to_string()]
        );
        assert_eq!(
            firewall_engine_argv(&["rime", "firewall", "reload"]),
            vec!["reload".to_string()]
        );
        assert_eq!(
            firewall_engine_argv(&["rime", "firewall", "allow", "mdns"]),
            vec!["allow".to_string(), "mdns".to_string()]
        );
        assert_eq!(
            firewall_engine_argv(&["rime", "firewall", "deny", "mdns"]),
            vec!["deny".to_string(), "mdns".to_string()]
        );
    }

    #[test]
    fn a_service_name_is_passed_as_one_word_however_it_is_spelt() {
        // The helper takes this straight into a path under
        // /etc/rime/firewall.d and refuses it there. What must not happen on
        // THIS side is the name arriving split or reshaped, because then the
        // helper's own refusal is about a different string than the user typed.
        assert_eq!(
            firewall_engine_argv(&["rime", "firewall", "deny", "../../etc/issue"]),
            vec!["deny".to_string(), "../../etc/issue".to_string()]
        );
        assert_eq!(
            firewall_engine_argv(&["rime", "firewall", "deny", "a b"]),
            vec!["deny".to_string(), "a b".to_string()]
        );
    }

    // ── §9: the resolver's escape hatch ─────────────────────────────────────

    #[test]
    fn no_source_is_named_unless_the_user_named_one() {
        // The empty case is the one that matters: `rime install htop` must
        // reach the engine exactly as it did before the resolver existed, or
        // this is a behaviour change on a shipped command.
        assert_eq!(
            install_engine_argv(&["rime", "install", "htop"]),
            vec!["install".to_string(), "htop".to_string()]
        );
    }

    #[test]
    fn the_chosen_source_reaches_the_engine() {
        assert_eq!(
            install_engine_argv(&["rime", "install", "--source", "flatpak", "discord"]),
            vec![
                "install".to_string(),
                "discord".to_string(),
                "--source=flatpak".to_string(),
            ]
        );
        assert_eq!(
            install_engine_argv(&[
                "rime", "install", "--source", "capsule", "--env", "arch", "yay",
            ]),
            vec![
                "install".to_string(),
                "yay".to_string(),
                "--source=capsule".to_string(),
                "--env=arch".to_string(),
            ]
        );
    }

    #[test]
    fn a_capsule_install_does_not_demand_root() {
        // Root has no capsules. Demanding root here would make the CLI ask for
        // a password and the engine then refuse the privileged invocation —
        // the user gets two refusals and no package.
        assert_eq!(
            privilege(&["rime", "install", "--source", "capsule", "htop"]),
            None
        );
        // Every other source still writes something the system owns.
        assert_eq!(
            privilege(&["rime", "install", "--source", "rpm", "htop"]),
            Some("install")
        );
        assert_eq!(
            privilege(&["rime", "install", "--source", "flatpak", "discord"]),
            Some("install")
        );
    }

    #[test]
    fn asking_what_would_happen_never_needs_a_password() {
        assert_eq!(privilege(&["rime", "resolve", "discord"]), None);
        match Cli::try_parse_from(["rime", "resolve", "discord"])
            .expect("parses")
            .command
        {
            Cmd::Resolve { name } => assert_eq!(name, "discord"),
            _ => panic!("not a resolve"),
        }
    }

    fn privilege(argv: &[&str]) -> Option<&'static str> {
        privileged_verb(&Cli::try_parse_from(argv).expect("parses").command)
    }

    #[test]
    fn install_is_still_a_root_only_verb() {
        // Adding a flag must not accidentally move `install` out of the
        // privileged set: it writes an extension and re-merges /usr.
        assert_eq!(
            privilege(&["rime", "install", "--allow-unsigned", "./x.rpm"]),
            Some("install")
        );
        assert_eq!(privilege(&["rime", "remove", "htop"]), Some("remove"));
        assert_eq!(privilege(&["rime", "pkg", "upgrade"]), Some("pkg upgrade"));
    }

    #[test]
    fn reading_never_needs_a_password() {
        // Each of these is driven from the desktop as the session user. A
        // password prompt here is not a security improvement, it is a shell
        // that stops working.
        for argv in [
            vec!["rime", "search", "htop"],
            vec!["rime", "pkg", "list"],
            vec!["rime", "pkg", "verify"],
            vec!["rime", "status"],
            vec!["rime", "tier"],
            vec!["rime", "fan", "status"],
        ] {
            assert_eq!(privilege(&argv), None, "{argv:?} demanded root");
        }
    }

    // ── rime devices ────────────────────────────────────────────────────────

    fn devices(argv: &[&str]) -> Vec<String> {
        match Cli::try_parse_from(argv).expect("parses").command {
            Cmd::Devices { area } => devices_argv(area),
            _ => panic!("not a devices verb"),
        }
    }

    #[test]
    fn every_area_reaches_the_helper_by_the_name_the_helper_dispatches_on() {
        // The helper's `case` arms are these words. A rename on one side is not
        // a compile error on the other: it is `rime devices dock` printing the
        // usage text and exiting 2.
        for (typed, sent) in [
            ("print", "print"),
            ("scan", "scan"),
            ("share", "share"),
            ("media", "media"),
            ("network", "network"),
            ("bluetooth", "bluetooth"),
            ("dock", "dock"),
            ("all", "all"),
        ] {
            assert_eq!(devices(&["rime", "devices", typed]), vec![sent]);
        }
    }

    #[test]
    fn no_area_asks_for_everything_rather_than_for_nothing() {
        // The helper defaults to `all` on its own, but only because nothing is
        // passed. Sending an empty argv would work by coincidence; sending the
        // word means the default survives a change to the helper's dispatch.
        assert_eq!(devices(&["rime", "devices"]), vec!["all"]);
    }

    #[test]
    fn an_area_that_does_not_exist_is_refused_before_anything_runs() {
        // A free-string argument would hand "dcok" to the helper, which prints
        // usage to stderr and exits 2. clap answers with the list instead, and
        // no subprocess starts.
        assert!(Cli::try_parse_from(["rime", "devices", "dcok"]).is_err());
    }

    #[test]
    fn reading_the_devices_is_never_a_privileged_verb() {
        // Root would report something different rather than something more: it
        // walks through the 0000 directory whose refusal is the answer, it has
        // no seat, and its bluetoothctl sees a different set of paired devices
        // than the session asking the question.
        for argv in [
            vec!["rime", "devices"],
            vec!["rime", "devices", "media"],
            vec!["rime", "devices", "network"],
            vec!["rime", "devices", "dock"],
        ] {
            assert_eq!(privilege(&argv), None, "{argv:?} demanded root");
        }
    }

    // ── rime plugin (§16) ───────────────────────────────────────────────────

    fn plugin(argv: &[&str]) -> Vec<String> {
        match Cli::try_parse_from(argv).expect("parses").command {
            Cmd::Plugin { cmd } => plugin_argv(cmd),
            _ => panic!("not a plugin verb"),
        }
    }

    #[test]
    fn the_plugin_verbs_reach_the_helper_unchanged() {
        assert_eq!(plugin(&["rime", "plugin", "list"]), vec!["list"]);
        assert_eq!(
            plugin(&["rime", "plugin", "list", "--json"]),
            vec!["list", "--json"]
        );
        assert_eq!(
            plugin(&["rime", "plugin", "info", "rime-worldclock"]),
            vec!["info", "rime-worldclock"]
        );
        assert_eq!(
            plugin(&["rime", "plugin", "enable", "rime-worldclock"]),
            vec!["enable", "rime-worldclock"]
        );
        assert_eq!(
            plugin(&["rime", "plugin", "disable", "rime-worldclock"]),
            vec!["disable", "rime-worldclock"]
        );
    }

    #[test]
    fn a_plugin_id_is_passed_through_and_never_interpreted_here() {
        // The id is validated by the helper — for path safety in shell, and
        // against rime-shell's own `validId` through node. This side must not
        // pre-filter it: a CLI that silently dropped or rewrote an id would
        // make the helper's refusal unreachable, and the refusal is the thing
        // that keeps a traversal out of a filesystem path.
        assert_eq!(
            plugin(&["rime", "plugin", "info", "../../etc/passwd"]),
            vec!["info", "../../etc/passwd"]
        );
    }

    #[test]
    fn plugins_are_never_a_privileged_verb() {
        // Every path `rime plugin` touches is under the invoking user's
        // ~/.config/rime-shell, which is the directory Rime Shell itself
        // reads. A root `rime plugin disable` would move root's plugins and
        // leave the user's alone — a command that reports success and changes
        // nothing the user can see.
        for argv in [
            vec!["rime", "plugin", "list"],
            vec!["rime", "plugin", "info", "x"],
            vec!["rime", "plugin", "enable", "x"],
            vec!["rime", "plugin", "disable", "x"],
        ] {
            assert_eq!(privilege(&argv), None, "{argv:?} demanded root");
        }
    }

    #[test]
    fn the_plugin_helper_is_an_absolute_path_in_libexec() {
        // Not a PATH lookup. `rime plugin` drives a shipped program, and
        // resolving it through PATH would let anything on the user's PATH
        // answer for the shell's plugin rules.
        assert!(ops::PLUGIN_ENGINE.starts_with('/'));
        assert_ne!(ops::PLUGIN_ENGINE, ops::ENV_ENGINE);
        assert_ne!(ops::PLUGIN_ENGINE, ops::PKG_ENGINE);
    }

    // ── rime env (§8 capsules) ──────────────────────────────────────────────

    fn env(argv: &[&str]) -> Vec<String> {
        match Cli::try_parse_from(argv).expect("parses").command {
            Cmd::Env { cmd } => env_argv(cmd),
            _ => panic!("not an env verb"),
        }
    }

    #[test]
    fn env_create_passes_the_name_and_nothing_it_was_not_given() {
        assert_eq!(env(&["rime", "env", "create", "fedora"]), vec!["create", "fedora"]);
    }

    #[test]
    fn the_device_profile_reaches_the_engine() {
        // The one flag that decides whether a capsule can see the GPU. A
        // silently dropped `--gpu` produces a capsule that looks right and
        // cannot compute, which is a bug report about drivers.
        assert_eq!(
            env(&["rime", "env", "create", "ml", "--gpu", "amd"]),
            vec!["create", "ml", "--gpu=amd"]
        );
        assert_eq!(
            env(&["rime", "env", "create", "box", "--image", "docker.io/library/ubuntu:24.04"]),
            vec!["create", "box", "--image=docker.io/library/ubuntu:24.04"]
        );
    }

    #[test]
    fn a_trailing_command_is_separated_from_the_engines_own_flags() {
        // Without the `--` the engine cannot tell a command's flags from its
        // own, and clap has already consumed the separator the user typed.
        assert_eq!(
            env(&["rime", "env", "exec", "box", "ls", "-l"]),
            vec!["exec", "box", "--", "ls", "-l"]
        );
        assert_eq!(
            env(&["rime", "env", "enter", "box", "--", "bash", "-lc", "echo hi"]),
            vec!["enter", "box", "--", "bash", "-lc", "echo hi"]
        );
    }

    #[test]
    fn entering_without_a_command_asks_for_a_login_shell() {
        // `enter box --` with an empty command must not reach the engine, or it
        // would report a usage error for a request that is perfectly valid.
        assert_eq!(env(&["rime", "env", "enter", "box"]), vec!["enter", "box"]);
    }

    #[test]
    fn removing_a_capsule_does_not_inherit_force() {
        assert_eq!(env(&["rime", "env", "rm", "box"]), vec!["rm", "box"]);
        assert_eq!(
            env(&["rime", "env", "rm", "box", "--force", "--keep-home"]),
            vec!["rm", "box", "--keep-home", "--force"]
        );
    }

    #[test]
    fn the_gui_export_reaches_the_engine_with_both_halves() {
        // §8's launcher integration. The application name is a positional, not
        // a flag, and the engine refuses anything that is not a bare name — so
        // a dropped argument here would become a usage error rather than an
        // export of something else.
        assert_eq!(
            env(&["rime", "env", "export", "py", "gimp"]),
            vec!["export", "py", "gimp"]
        );
        assert_eq!(
            env(&["rime", "env", "unexport", "py", "gimp"]),
            vec!["unexport", "py", "gimp"]
        );
        assert_eq!(env(&["rime", "env", "exports", "py"]), vec!["exports", "py"]);
    }

    #[test]
    fn provisioning_a_language_names_the_language_and_not_a_capsule() {
        // The capsule a language lives in is the ENGINE's decision — c and cpp
        // share one, javascript and typescript share one — so the CLI must not
        // pass a capsule name here or it would be a second answer to the same
        // question.
        assert_eq!(
            env(&["rime", "env", "provision", "rust"]),
            vec!["provision", "rust"]
        );
        assert_eq!(env(&["rime", "env", "languages"]), vec!["languages"]);
    }

    #[test]
    fn capsules_are_never_a_privileged_verb() {
        // Capsules are rootless per-user containers. If `rime env` ever landed
        // in the privileged set it would create them under
        // /var/lib/containers, shared by every account, and need an
        // authentication prompt to enter a shell.
        //
        // `export` and `provision` are in this list for a sharper reason than
        // the others: both are reachable from `rime apply`, and the blueprint's
        // whole claim to never raising an authentication prompt is that it
        // converges the privilege domain it is already in. A privileged capsule
        // verb would break that claim from the outside.
        for argv in [
            vec!["rime", "env", "create", "fedora"],
            vec!["rime", "env", "rm", "fedora"],
            vec!["rime", "env", "install", "fedora", "htop"],
            vec!["rime", "env", "enter", "fedora"],
            vec!["rime", "env", "export", "fedora", "gimp"],
            vec!["rime", "env", "unexport", "fedora", "gimp"],
            vec!["rime", "env", "provision", "rust"],
        ] {
            assert_eq!(privilege(&argv), None, "{argv:?} demanded root");
        }
    }
}
