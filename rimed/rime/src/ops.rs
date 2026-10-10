//! Non-D-Bus operations: shelling out to bootc/ostree/fwupd for update,
//! rollback, pin and changelog, plus local read-only rendering used both as a
//! daemon-less fallback and by `rime fingerprint`/`doctor`.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use rimed_core::tier::Tier;
use rimed_core::{Fingerprint, Profile, ProfileSet, Selection};

// ── Root gating ──────────────────────────────────────────────────────────────
//
// `update`, `rollback` and `pin` drive bootc/ostree, which write to /ostree and
// /boot. Run as an ordinary user they used to reach the external tool and fail
// there, with whatever wording bootc chose — typically a bare permission error
// that says nothing about sudo. Worse, `rime update`'s firmware half ran
// afterwards regardless, so the command printed a wall of fwupd output and
// could still exit 0 having updated nothing.
//
// So these verbs now refuse up front, before any hardware probe, D-Bus connect
// or subprocess, and say exactly what to type instead.
//
// This is deliberately NOT applied to the whole CLI. `rime tier`, `status`,
// `battery`, `fan`, `game` and `doctor` are reached by Rime Shell's power tab
// as the session user — mutations go through rimed's polkit-authorised D-Bus
// API, which is precisely how an unprivileged desktop is supposed to change
// power state. A blanket root requirement would break the desktop's power
// controls to fix a message.

/// Effective UID, read from `/proc/self/status`.
///
/// `/proc` rather than a libc call: this crate has no C dependency and adding
/// one for a single integer is not worth it on a Linux-only OS CLI. Returns
/// `None` if /proc is unavailable or malformed, which the caller treats as
/// "not root" — failing closed.
pub fn effective_uid() -> Option<u32> {
    parse_effective_uid(&std::fs::read_to_string("/proc/self/status").ok()?)
}

/// Pull the effective UID out of `/proc/self/status`.
///
/// The line is `Uid:\t<real>\t<effective>\t<saved>\t<fs>`. The EFFECTIVE id is
/// the one that matters: it is what the kernel checks, and it is what differs
/// under a setuid path.
pub fn parse_effective_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|rest| rest.split_whitespace().nth(1))
        .and_then(|euid| euid.parse().ok())
}

/// How the user invoked us, rendered as the sudo command to run instead.
///
/// The full argument list is echoed, not a bare `sudo rime <verb>`: someone who
/// typed `rime update --check --skip-firmware` should be able to copy one line,
/// not reconstruct their own flags.
fn sudo_reinvocation(argv: &[String]) -> String {
    let mut out = String::from("sudo rime");
    for a in argv.iter().skip(1) {
        out.push(' ');
        out.push_str(a);
    }
    out
}

/// The refusal text for a root-only verb.
///
/// The parenthetical is verb-specific because the generic one used to explain
/// package installs in terms of bootc writing to /ostree and /boot, which is
/// simply not what happens — `rime install` never touches either.
fn root_required_message(verb: &str, argv: &[String]) -> String {
    let why = if verb.starts_with("install")
        || verb.starts_with("remove")
        || verb.starts_with("pkg")
    {
        "it writes the system extension under /var/lib and asks systemd to \
         re-merge /usr"
    } else {
        "bootc writes to /ostree and /boot"
    };
    format!(
        "rime: '{verb}' changes the booted system and must run as root.\n\
         \x20      try:  {}\n\
         \x20      (being in the wheel group is not enough — {why},\n\
         \x20       so the command itself has to run with privileges.)",
        sudo_reinvocation(argv)
    )
}

/// Refuse a root-only verb when we are not root.
///
/// Returns `Err(exit_code)` after printing the refusal, so callers can bail
/// before touching anything.
pub fn require_root(verb: &str) -> Result<(), i32> {
    if effective_uid() == Some(0) {
        return Ok(());
    }
    let argv: Vec<String> = std::env::args().collect();
    eprintln!("{}", root_required_message(verb, &argv));
    Err(1)
}

/// Run an external command, streaming its output. Returns Ok(code) or a clear
/// message if the binary is missing — never panics.
pub fn run(program: &str, args: &[&str]) -> Result<i32, String> {
    eprintln!("rime: running: {program} {}", args.join(" "));
    match Command::new(program).args(args).status() {
        Ok(status) => Ok(status.code().unwrap_or(-1)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("'{program}' not found on PATH ({e})"))
        }
        Err(e) => Err(format!("failed to run '{program}': {e}")),
    }
}

/// Capture stdout of a command (trimmed). None if it cannot run.
fn capture(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `rime pin` -> pin the current (booted) deployment so an update can't garbage
/// collect the rollback target.
pub fn pin() -> i32 {
    match run("ostree", &["admin", "pin", "0"]) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("rime: pin failed: {e}");
            1
        }
    }
}

/// `rime rollback` -> boot the previous deployment next reboot.
pub fn rollback() -> i32 {
    match run("bootc", &["rollback"]) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("rime: rollback failed: {e}");
            1
        }
    }
}

// ── fwupd exit codes ─────────────────────────────────────────────────────────
// fwupdmgr does NOT use shell conventions. It returns:
//     0  success
//     1  failure
//     2  nothing to do        (EXIT_NOTHING_TO_DO)
//     3  nothing found        (EXIT_NOT_FOUND — e.g. no LVFS-covered devices)
// Both 2 and 3 are ordinary, expected outcomes on a laptop that is already
// current, and treating them as failure is how `rime update` would come to
// report an error on the most common path of all.
const FWUPD_NOTHING_TO_DO: i32 = 2;
const FWUPD_NOT_FOUND: i32 = 3;

fn fwupd_idle(code: i32) -> bool {
    code == FWUPD_NOTHING_TO_DO || code == FWUPD_NOT_FOUND
}


// ── ostree fsync during a pull ───────────────────────────────────────────────
//
// MEASURED ON THE AUTHOR'S L16, because "updates feel slow" deserved a number
// rather than a guess:
//
//   single-stream download from GHCR ... 14.6 MiB/s   (51 ms RTT, curl)
//   6 parallel streams ................. 49.8 MiB/s
//   what `rime update` actually got ....  ~8 MiB/s
//   disk write throughput .............. 999 MB/s
//   fsync cost per small file ..........  2.98 ms   (131x slower than without)
//   objects in the ostree repo ......... 179,365
//
// The network was never the limit and neither was the disk. `core.fsync` is
// unset in the repo, which means ostree fsyncs EVERY object it writes, and
// 179k objects x 2.98 ms is ~534 s of pure fsync serialised against ~372 s of
// download. That models to 6.0 MiB/s against the ~8 observed — fsync is the
// dominant cost of an update, not bandwidth.
//
// So it is turned off for the duration of the pull and restored afterwards.
//
// THE TRADE, stated plainly: fsync is what guarantees a written object survives
// a power loss. With it off, losing power mid-pull can leave a corrupt object in
// the repo. What that costs is bounded — the BOOTED deployment is never touched
// by a pull, ostree checksums every object it reads, and the remedy is to pull
// again (`ostree fsck` reports it, `rime update` re-fetches). Weighed against
// halving the time the machine spends updating, on a laptop with a battery, that
// is the right default. `rime update --fsync` keeps it on.
const OSTREE_REPO: &str = "/ostree/repo";
/// Records that a pull turned fsync off, so a run killed mid-pull can put it
/// back rather than leaving the repo permanently unsafe and silent about it.
/// Under /var so it survives the reboot a crash might cause.
const FSYNC_MARKER: &str = "/var/lib/rimeos/fsync-disabled";

/// Reads `core.fsync`; `None` when unset (ostree's default, which is on).
fn ostree_fsync_setting() -> Option<String> {
    capture("ostree", &["config", "--repo", OSTREE_REPO, "get", "core.fsync"])
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn ostree_fsync_write(value: Option<&str>) -> bool {
    let args: Vec<&str> = match value {
        Some(v) => vec!["config", "--repo", OSTREE_REPO, "set", "core.fsync", v],
        None => vec!["config", "--repo", OSTREE_REPO, "unset", "core.fsync"],
    };
    matches!(
        Command::new("ostree").args(&args).output(),
        Ok(o) if o.status.success()
    )
}

/// Restores fsync if a previous run died with it disabled. Called before every
/// pull, so the unsafe window can never outlive one update by more than one.
fn recover_stale_fsync() {
    if !Path::new(FSYNC_MARKER).exists() {
        return;
    }
    let prior = std::fs::read_to_string(FSYNC_MARKER).unwrap_or_default();
    let prior = prior.trim();
    eprintln!(
        "rime: a previous update was interrupted with ostree fsync disabled — restoring it"
    );
    let restored = if prior.is_empty() {
        ostree_fsync_write(None)
    } else {
        ostree_fsync_write(Some(prior))
    };
    if restored {
        let _ = std::fs::remove_file(FSYNC_MARKER);
    } else {
        eprintln!("rime: WARNING could not restore core.fsync — run: sudo ostree config --repo={OSTREE_REPO} unset core.fsync");
    }
}

/// Disables `core.fsync` while alive, restores it on drop.
struct FsyncGuard {
    prior: Option<String>,
    active: bool,
}

impl FsyncGuard {
    /// `None` when fsync was left alone (asked not to, or ostree unavailable).
    fn disable() -> FsyncGuard {
        recover_stale_fsync();
        let prior = ostree_fsync_setting();
        // Already false — someone set it deliberately. Leave it, and leave no
        // marker, so we never "restore" a choice that was not ours.
        if prior.as_deref() == Some("false") {
            return FsyncGuard { prior: None, active: false };
        }
        if let Some(dir) = Path::new(FSYNC_MARKER).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // The marker is written BEFORE the change, so a crash in between leaves
        // a spurious marker (harmless: recovery just re-asserts the prior value)
        // rather than a disabled fsync nobody knows about.
        let _ = std::fs::write(FSYNC_MARKER, prior.clone().unwrap_or_default());
        if ostree_fsync_write(Some("false")) {
            eprintln!("rime: ostree fsync disabled for this pull (restored afterwards)");
            FsyncGuard { prior, active: true }
        } else {
            let _ = std::fs::remove_file(FSYNC_MARKER);
            eprintln!("rime: could not disable ostree fsync — the pull will be slower");
            FsyncGuard { prior: None, active: false }
        }
    }
}

impl Drop for FsyncGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if ostree_fsync_write(self.prior.as_deref()) {
            let _ = std::fs::remove_file(FSYNC_MARKER);
            eprintln!("rime: ostree fsync restored");
        } else {
            eprintln!(
                "rime: WARNING could not restore core.fsync — run: sudo ostree config --repo={OSTREE_REPO} unset core.fsync"
            );
        }
    }
}

/// What `rime update` should do this run.
#[derive(Default, Clone, Copy)]
pub struct UpdateOptions {
    /// Report what is available; download and stage nothing.
    pub check: bool,
    /// The live-update engine's switches (`--plan`, `--live-only`, `--no-live`).
    pub live: crate::live::LiveOptions,
    /// Skip the firmware (fwupd) pass entirely.
    pub skip_firmware: bool,
    /// Run only the firmware pass; leave the OS image alone.
    pub firmware_only: bool,
    /// Keep ostree's per-object fsync on during the pull. Slower — see the
    /// FsyncGuard notes — but durable against a power loss mid-update.
    pub keep_fsync: bool,
    /// Skip refreshing user packages (`rime install`) from the repositories.
    pub skip_packages: bool,
    /// Skip updating Flatpak applications.
    pub skip_flatpak: bool,
    /// Ignore §26's rollout stop and update anyway.
    pub force: bool,
    /// Deploy an image whose signature §27's gate refused.
    ///
    /// Separate from `force` because the two gates answer different
    /// questions, and a machine whose last update left it broken is not a
    /// machine that should also stop checking who signed the next one.
    pub allow_unverified: bool,
}

/// The system-extension package engine behind `rime install`/`remove`/`pkg`.
/// A constant so the CLI, `rime update` and rime-sysext-rebuild.service can
/// never disagree about where it lives.
pub const PKG_ENGINE: &str = "/usr/libexec/rime-pkg";

/// State file written by the engine. Its absence means "this machine has no
/// user packages", which is the common case and must cost nothing.
const PKG_STATE: &str = "/var/lib/rime/pkg/state.json";

/// Where the engine records installed AppImages. Read here for one reason: an
/// AppImage is NOT part of the system extension and writes no `PKG_STATE`, so
/// a machine whose only user software is an AppImage has none of that file —
/// and `packages_pass` below would then never run the engine at all, never
/// print the line that says AppImages are pinned, and leave the user to find
/// that out from a security advisory. `docs/packages.md` states the pinning as
/// something `rime update` tells you; this is what makes that true.
const PKG_APPIMAGE_DIR: &str = "/var/lib/rime/appimage";

/// The capsule engine behind `rime env` (§8). Same shape as `PKG_ENGINE`: the
/// policy lives in one shipped program so the CLI, the resolver and anything
/// the shell drives cannot disagree about what a capsule is.
pub const ENV_ENGINE: &str = "/usr/libexec/rime-env";

/// The disposable-capsule engine behind `rime disposable` (§19).
///
/// A constant, like [`ENV_ENGINE`] and [`PKG_ENGINE`], and for the same reason:
/// a caller-controlled variable naming a program is a hole even in an
/// unprivileged command. It is a separate program from `rime-env` and a *mode
/// of the same mechanism*: every environment it makes is an ordinary capsule
/// created through `rime-env`, so `rime env list` sees it and `podman ps` sees
/// it, and Rime has not grown a second container runtime.
pub const DISPOSABLE_ENGINE: &str = "/usr/libexec/rime-disposable";

/// The account engine behind `rime user` (P2-016).
///
/// A constant, like every other engine path here, and this one more than most:
/// it runs `useradd`, `userdel` and `systemctl enable` as root, so a
/// caller-controlled variable naming it would be a way to have `sudo rime
/// user` execute somebody else's program.
pub const USER_ENGINE: &str = "/usr/libexec/rime-user";

/// The virtualization engine behind `rime vm` (P2-008).
///
/// A constant, like every other engine path here, and for the same reason: a
/// caller-controlled variable naming a program is a hole. This one defines
/// libvirt domains and deletes disk images, so it is the last one that should
/// be reachable through the environment.
pub const VM_ENGINE: &str = "/usr/libexec/rime-vm";

/// P2-012's browser capsule engine.
pub const BROWSER_ENGINE: &str = "/usr/libexec/rime-browser";

/// The plugin CLI behind `rime plugin` (§16).
///
/// A constant, not an overridable variable — the same rule as [`PKG_ENGINE`]
/// and [`ENV_ENGINE`]. Unlike `rime apply`'s capsule step there is no test that
/// needs to redirect this one: `tests/test-rime-plugin.sh` drives the shipped
/// script directly, the way the capsule and package suites do.
pub const PLUGIN_ENGINE: &str = "/usr/libexec/rime-plugin";

/// `rime plugin …`.
///
/// Unprivileged, and structurally so: every path it touches is under the
/// invoking user's `~/.config/rime-shell`, which is the same directory Rime
/// Shell reads. A root `rime plugin disable` would move a plugin belonging to
/// root and leave the user's alone, which is a command that reports success
/// and changes nothing the user can see.
/// The firewall helper behind `rime firewall`.
///
/// Same shape as [`PKG_ENGINE`] and [`PLUGIN_ENGINE`], and for the same reason:
/// a caller-controlled variable naming a program that runs under sudo is a hole
/// whatever the program does. The default-drop policy itself is a shipped
/// nftables file this helper does not edit — it manages the exception list in
/// front of it, so a malformed exception cannot take the base policy with it.
pub const FIREWALL_ENGINE: &str = "/usr/libexec/rime-firewall";

/// The device enumeration behind `rime devices`.
///
/// A constant for the same reason as [`FIREWALL_ENGINE`] and [`PKG_ENGINE`].
/// This one is never run under sudo — it reads, and every verb it has is
/// unprivileged — but a caller-controlled variable naming a program is a hole
/// whatever the program does, and the rule is worth more than the exception.
pub const DEVICES_ENGINE: &str = "/usr/libexec/rime-devices";

/// `rime devices …`.
///
/// Unprivileged, and it must stay that way. Running it as root would change
/// what it reports rather than reveal more: root walks through the 0000
/// directory whose refusal is the interesting answer, root has no seat, and
/// root's `bluetoothctl` sees a different set of paired devices than the user
/// whose desktop is asking. A diagnostic that has to be run as root to be
/// believed cannot tell a user why their own session cannot see a device.
pub fn devices(args: &[String]) -> i32 {
    match Command::new(DEVICES_ENGINE).args(args).status() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(e) => {
            eprintln!("rime: cannot run the device enumeration: {e}");
            eprintln!(
                "rime: no device helper on this system — it predates `rime devices`.\n\
                 \x20      run `sudo rime update` first."
            );
            1
        }
    }
}

/// `rime firewall …`.
pub fn firewall(args: &[String]) -> i32 {
    match Command::new(FIREWALL_ENGINE).args(args).status() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(e) => {
            eprintln!("rime: cannot run the firewall helper: {e}");
            eprintln!(
                "rime: no firewall helper on this system — it predates `rime firewall`.\n\
                 \x20      run `sudo rime update` first."
            );
            1
        }
    }
}

pub fn plugin(args: &[String]) -> i32 {
    match Command::new(PLUGIN_ENGINE).args(args).status() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(e) => {
            eprintln!("rime: cannot run the plugin helper: {e}");
            eprintln!(
                "rime: no plugin helper on this system — it predates `rime plugin`.\n\
                 \x20      run `sudo rime update` first."
            );
            1
        }
    }
}

/// `rime env …`.
///
/// Unprivileged on purpose, and it must stay that way: capsules are rootless
/// per-user podman containers. Routing this through sudo would put their images
/// under /var/lib/containers, share one environment between every account on
/// the machine, and need an authentication prompt to enter a shell.
///
/// `enter` replaces this process rather than waiting on a child, so an
/// interactive capsule shell gets the terminal, the signals and the exit status
/// directly.
pub fn env(args: &[String]) -> i32 {
    match Command::new(ENV_ENGINE).args(args).status() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(e) => {
            eprintln!("rime: cannot run the capsule engine: {e}");
            eprintln!(
                "rime: no capsule engine on this system — it predates `rime env`.\n\
                 \x20      run `sudo rime update` first."
            );
            1
        }
    }
}

/// `rime disposable …`.
///
/// Unprivileged, structurally: a disposable capsule is a rootless per-user
/// container and its throwaway home is under the user's own state directory.
/// Running it as root would put the images under /var/lib/containers and need
/// an authentication prompt to enter a shell — and a "disposable" environment
/// that survives in root's storage is not disposable.
///
/// `status()` rather than `output()`: `run` gives the terminal to an
/// interactive capsule shell, and the teardown has to happen when that shell
/// exits.
/// `rime user …`.
///
/// `status()` rather than `output()` for the same reason as the rest: the
/// engine writes its refusals to stderr and the user needs to read them as
/// they happen, and `rime user list` is a table that should stream.
///
/// Not made privileged here. `rime user list` is deliberately usable by
/// anybody, and the engine refuses the verbs that need root with a sentence
/// naming sudo — which is a better failure than this binary deciding on the
/// caller's behalf that a read-only question needs a password.
pub fn user(args: &[String]) -> i32 {
    match Command::new(USER_ENGINE).args(args).status() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(e) => {
            eprintln!("rime: cannot run the account engine: {e}");
            eprintln!(
                "rime: no account engine on this system — it predates `rime user`.\n\
                 \x20      run `sudo rime update` first."
            );
            1
        }
    }
}

pub fn disposable(args: &[String]) -> i32 {
    match Command::new(DISPOSABLE_ENGINE).args(args).status() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(e) => {
            eprintln!("rime: cannot run the disposable engine: {e}");
            eprintln!(
                "rime: no disposable engine on this system — it predates `rime disposable`.\n\
                 \x20      run `sudo rime update` first."
            );
            1
        }
    }
}

/// `rime vm …`.
///
/// Unprivileged, structurally: every domain is a per-user one at
/// `qemu:///session` and every disk is under the user's own data directory.
/// Running this as root would put the domains in libvirt's system namespace,
/// where defining one needs polkit and where the `default` network is a host
/// bridge — the three things `rime vm` exists to avoid.
///
/// `status()` rather than `output()`: `rime vm console` hands the terminal to
/// a serial console the user detaches from with Ctrl-].
pub fn browser(args: &[String]) -> i32 {
    match Command::new(BROWSER_ENGINE).args(args).status() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(e) => {
            eprintln!("rime: cannot run the browser capsule engine: {e}");
            eprintln!(
                "rime: no browser capsule engine on this system — it predates `rime browser`.\n\
                 \x20      run `sudo rime update` first."
            );
            1
        }
    }
}

pub fn vm(args: &[String]) -> i32 {
    match Command::new(VM_ENGINE).args(args).status() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(e) => {
            eprintln!("rime: cannot run the virtualization engine: {e}");
            eprintln!(
                "rime: no VM engine on this system — it predates `rime vm`.\n\
                 \x20      run `sudo rime update` first."
            );
            1
        }
    }
}

/// `rime install` / `rime remove` / `rime search` / `rime pkg …`.
///
/// Deliberately a thin pass-through: the engine owns dependency resolution,
/// signature checking and the extension lifecycle, and duplicating any of that
/// here would just create two implementations to keep in sync.
pub fn pkg(args: &[String]) -> i32 {
    // No "rime: running: …" banner here, unlike the bootc/fwupd verbs: the
    // engine narrates its own work, and echoing the invocation on top of that
    // made read-only verbs like `rime pkg list` print noise before their output.
    match Command::new(PKG_ENGINE).args(args).status() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(e) => {
            eprintln!("rime: cannot run the package engine: {e}");
            eprintln!(
                "rime: no package engine on this system — it predates `rime install`.\n\
                 \x20      run `sudo rime update` first."
            );
            1
        }
    }
}

/// Refresh user packages during `rime update`.
///
/// User packages are ordinary Fedora RPMs, so they carry ordinary Fedora
/// security fixes. Updating the OS while leaving them pinned at whatever was
/// current on install day would quietly turn "the system is up to date" into a
/// half-truth.
fn packages_pass() -> i32 {
    if !Path::new(PKG_STATE).exists() && !has_appimages() {
        return 0;
    }
    match run(PKG_ENGINE, &["upgrade"]) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("rime: user packages skipped: {e}");
            0
        }
    }
}

/// Does this machine have at least one AppImage the engine installed?
///
/// One record per application, so a directory containing any `*.json` is the
/// question. Unreadable or missing answers false, which is the same direction
/// `PKG_STATE`'s absence answers: an extra engine invocation is cheap, but
/// refusing to look is not a reason to claim there is nothing there.
fn has_appimages() -> bool {
    let Ok(entries) = std::fs::read_dir(PKG_APPIMAGE_DIR) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_name()
            .to_str()
            .is_some_and(|n| n.ends_with(".json"))
    })
}

/// `rime update` -> pull a newer OS image, then refresh firmware via fwupd.
///
/// ── Why this is not just two `fwupdmgr` calls any more ──
/// The old version ran `fwupdmgr refresh --force` and then `fwupdmgr update -y`
/// unconditionally, every single time. `--force` means "ignore the cache age",
/// so every run re-downloaded the entire LVFS metadata index — tens of MB of
/// signed XML that fwupd itself only considers stale after 24 hours — and then
/// started a full device-enumeration update pass on a machine that, nine runs
/// in ten, had no firmware updates at all. On the author's L16 that was the
/// slowest part of `rime update` whenever the OS image was already current.
///
/// Now: refresh honours fwupd's own cache window, and the update pass runs only
/// after `get-updates` says there is something to install.
/// §27's enforcement pass: does the image this update would deploy actually
/// verify, and is this machine configured to care?
///
/// `Some(code)` means `update` stops here with that code.
///
/// Roadmap §27's producer half has worked for months — every published digest
/// is cosign-signed under a keyless GitHub identity and CI verifies its own
/// work before moving a tag — and none of it reached the machine. P1-047
/// landed the readout, so a Rime machine could finally say that nobody had
/// checked. This is the half that checks.
///
/// Three placement decisions, each of which the obvious alternative gets
/// wrong:
///
/// * **Before `record_update` and before `FsyncGuard::disable`.** Both of
///   those write machine state. A refusal that fired after them would have
///   recorded a health record for an update that never happened — which is
///   exactly what §26's rollout stop then reasons about — and left ostree's
///   per-object fsync switched off on a machine that is not updating.
/// * **On the digest the registry would SERVE, not the booted one.** `rime
///   trust --verify` answers "is what I am running signed"; a gate has to
///   answer "is what I am about to run signed". Those differ for the same tag
///   as a matter of routine here, because the four Rime tags are aliases for
///   one digest that moves on every successful main build. Verifying the
///   booted digest would wave an unsigned image through every time, while
///   printing "verified".
/// * **Not behind `--force`.** That flag is §26's escape, for a machine that
///   came back from its last update broken. Sharing it would mean anybody
///   working around a health stop silently stopped checking signatures too,
///   and the two have nothing to do with each other. `--allow-unverified` is
///   long on purpose.
fn trust_gate(allow_unverified: bool, target: Option<&str>) -> Option<i32> {
    let roots = crate::trust::Roots::from_env();
    let report = crate::trust::offline_report(&roots);
    // The origin read is handed to the gate rather than unwrapped here. An
    // early return on `image_error` is what made an unreadable /proc/cmdline
    // deploy an image nobody checked, under `signature=enforce`, while
    // printing a single line about it — the EACCES class this repository
    // swept fourteen readers for, one layer up.
    //
    // `target` is the image this update will MOVE to instead of the one the
    // machine tracks (the rename, see `rename_target`). The gate has to answer
    // for what is about to be deployed, so it verifies that name when there is
    // one.
    let g = crate::verify::gate(
        &roots,
        match (target, &report.image, &report.image_error) {
            (Some(t), _, _) => Ok(Some(t)),
            (None, Some(r), _) => Ok(Some(r.as_str())),
            (None, None, Some(e)) => Err(e.as_str()),
            (None, None, None) => Ok(None),
        },
    );
    let refusal = crate::verify::refusal(
        &g.verification,
        &g.enforcement,
        &g.decision,
        "--allow-unverified",
    );

    // A fixture root means every trust fact in play is a file somebody wrote
    // for a test. `bootc upgrade` is not run on the strength of those, in
    // either direction — which is also what makes all three decisions
    // exercisable through the real binary, headless, without a machine ever
    // staging an image.
    if roots.fixture.is_some() {
        print!(
            "{}",
            crate::verify::render(&g.verification, &g.enforcement, &g.decision)
        );
        if let Some(why) = &refusal {
            eprint!("{why}");
        }
        println!("rime: this program will not deploy on fixture facts");
        return Some(i32::from(g.decision.refuses() && !allow_unverified));
    }

    match refusal {
        None => {
            // Warnings are printed even when nothing is refused: "provenance
            // could not be established" is the normal state on every Rime
            // machine today, and a gate that stays silent about it is a gate
            // nobody knows is there.
            if let crate::verify::Decision::ProceedWithWarnings(w) = &g.decision {
                for line in w {
                    eprintln!("rime: {line}");
                }
            }
            None
        }
        Some(why) if allow_unverified => {
            // Asked for, so granted — and still printed in full. Skipping the
            // explanation would make `--allow-unverified` a way to not find
            // out what was wrong with the image you just deployed.
            eprint!("{why}");
            eprintln!(
                "rime: proceeding anyway because --allow-unverified was given."
            );
            None
        }
        Some(why) => {
            eprint!("{why}");
            Some(1)
        }
    }
}

/// The gate again, for the deployment `bootc upgrade --download-only` really
/// staged. `trust_gate` answered for the digest the registry would serve; a
/// tag can move between that lookup and the pull, so the digest that will
/// boot is verified by itself before anything queues or activates it.
/// Ok(true) verified; Ok(false) did not verify and `--allow-unverified` was
/// given (the caller records that, so nothing later trusts it as verified).
pub(crate) fn verify_staged(allow_unverified: bool, reference: &str, digest: &str) -> Result<bool, i32> {
    let roots = crate::trust::Roots::from_env();
    if roots.fixture.is_some() {
        println!("rime: this program will not deploy on fixture facts");
        return Err(1);
    }
    let enforcement = crate::verify::enforcement(&roots);
    let verification = crate::verify::verify_image(&roots, reference, digest);
    let decision = crate::verify::decide(&verification, &enforcement);
    match crate::verify::refusal(&verification, &enforcement, &decision, "--allow-unverified") {
        None => {
            if let crate::verify::Decision::ProceedWithWarnings(w) = &decision {
                for line in w {
                    eprintln!("rime: {line}");
                }
            }
            Ok(true)
        }
        Some(why) if allow_unverified => {
            eprint!("{why}");
            eprintln!("rime: proceeding anyway because --allow-unverified was given.");
            Ok(false)
        }
        Some(why) => {
            eprint!("{why}");
            Err(1)
        }
    }
}

/// The in-place move from ostree + GRUB to composefs + systemd-boot.
///
/// Returns true only when this machine actually migrated, in which case the
/// caller must NOT also run `bootc upgrade` in the same invocation.
///
/// Exit codes are the engine's: 0 migrated, 10 refused (and it said why, at
/// length, on stderr), anything else failed (likewise). Neither of the last
/// two is fatal to `rime update`: the machine is on GRUB and GRUB works.
fn migrate_boot_path() -> bool {
    const ENGINE: &str = "/usr/libexec/rime-boot-migrate";
    if !Path::new(ENGINE).exists() {
        return false;
    }
    match run(ENGINE, &["auto"]) {
        Ok(0) => true,
        // 3 is "there is nothing to do here" — this machine has already
        // migrated, or is booted on the new path. Silent on purpose: a
        // migrated machine printing four lines about staying on GRUB on every
        // single update would be worse than saying nothing.
        Ok(3) => false,
        Ok(10) => {
            eprintln!(
                "rime: this machine stays on GRUB for now — see the reason above, and\n\
                 rime: `rime-boot-migrate precheck` to re-check it at any time."
            );
            false
        }
        Ok(code) => {
            eprintln!(
                "rime: the boot-path migration did not complete (exit {code}). Nothing was\n\
                 rime: committed: this machine still boots the way it did."
            );
            false
        }
        Err(e) => {
            eprintln!("rime: could not run the boot-path migration: {e}");
            false
        }
    }
}

/// The tail of `update`: the passes that are independent of the OS image.
/// Factored out so the migration can return early without skipping them.
fn finish_update(started: Instant, mut worst: i32, opts: &UpdateOptions) -> i32 {
    if !opts.skip_packages && !opts.firmware_only {
        worst = worst.max(packages_pass());
    }
    if !opts.skip_flatpak && !opts.firmware_only {
        worst = worst.max(flatpak_pass());
    }
    if !opts.skip_firmware {
        worst = worst.max(firmware_pass());
    }
    println!(
        "rime: update finished in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    worst
}

/// The image name every Rime machine was installed tracking, and the one it
/// moves to with the rebrand to Rime OS (2026-09-28).
///
/// GHCR does not redirect a renamed package, so a machine keeps pulling the
/// name in its origin until something moves it. The workflow publishes every
/// build under both names for the transition; this is the something. It moves
/// a machine on its next update once the new name serves the machine's tag,
/// and until then it does nothing at all.
const OLD_IMAGE: &str = "ghcr.io/andrenijman/apex-os";  // rime-rename: keep
const NEW_IMAGE: &str = "ghcr.io/andrenijman/rime-os";

/// The reference a machine following `current` should move to, if any.
///
/// Only a TAG of the old name moves. A digest pin stays: whoever pinned a
/// digest chose that exact image. A fork's image, or a machine already on the
/// new name, is left alone.
fn renamed_reference(current: &str) -> Option<String> {
    if current.contains('@') {
        return None;
    }
    let tag = current.strip_prefix(OLD_IMAGE)?.strip_prefix(':')?;
    if tag.is_empty() || tag.contains('/') {
        return None;
    }
    Some(format!("{NEW_IMAGE}:{tag}"))
}

/// Whether the registry already serves `reference`.
///
/// Asked before every move, so a machine never switches to a name nothing has
/// been published under. Until the repository is renamed that is every machine
/// on every update, and the answer costs one manifest request. A fixture root
/// answers from a file instead of the network.
fn published(reference: &str) -> bool {
    let roots = crate::trust::Roots::from_env();
    if let Some(root) = &roots.fixture {
        return root.join("registry/renamed-published").exists();
    }
    capture(
        "skopeo",
        &["inspect", "--raw", "--no-tags", &format!("docker://{reference}")],
    )
    .is_some()
}

/// The image this update moves the machine to, or `None` to update in place.
fn rename_target() -> Option<String> {
    let current = crate::channel::booted_reference().ok()?;
    let target = renamed_reference(&current)?;
    published(&target).then_some(target)
}

pub fn update(opts: UpdateOptions) -> i32 {
    let started = Instant::now();
    let mut worst = 0;

    if opts.check {
        if !opts.firmware_only {
            match run("bootc", &["upgrade", "--check"]) {
                Ok(code) => worst = worst.max(code),
                Err(e) => {
                    eprintln!("rime: cannot check for an OS update: {e}");
                    worst = 1;
                }
            }
            if let Some(t) = rename_target() {
                println!("apex: APEX is now Rime OS: the next update moves this machine to {t}");  // rime-rename: keep
            }
        }
        if !opts.skip_firmware {
            match run("fwupdmgr", &["get-updates"]) {
                Ok(code) if fwupd_idle(code) => {
                    println!("rime: no firmware updates for this machine")
                }
                Ok(code) => worst = worst.max(code),
                Err(e) => eprintln!("rime: firmware check skipped: {e}"),
            }
        }
        return worst;
    }

    // §26's rollout stop, and the reason it lives here rather than in the
    // channel verb: a stop nobody's update path consults is a report. The gate
    // permits every uncertain state — an unreadable file, a digest it could not
    // resolve, an update that has not been rebooted into — and refuses exactly
    // one: this machine took the last update and came back with a regression
    // an image change could have caused. Advancing it again is how one bad
    // release becomes two, and the user is at the keyboard of the machine that
    // would do it.
    if !opts.firmware_only && !opts.force {
        if let Some(why) = crate::channel::halt_reason() {
            eprint!("{why}");
            return 1;
        }
    }

    // §27's signature gate. Deliberately after §26's stop, which is a local
    // file read and costs nothing, and deliberately before `record_update`
    // and `FsyncGuard::disable` below, which both write. See `trust_gate`.
    // Decided once, before the gate, so the gate verifies the image that will
    // actually be deployed and the image step deploys the image that was
    // verified.
    let target = if opts.firmware_only { None } else { rename_target() };
    if !opts.firmware_only {
        if let Some(code) = trust_gate(opts.allow_unverified, target.as_deref()) {
            return code;
        }
    }

    // §26's staged rollout. The publisher's ramp, read from a signed document
    // in the registry the machine already contacts — see
    // `rimed_core::channel::decide_rollout` for why it is not a label and
    // `docs/update-channels.md` for what that costs.
    //
    // Not an error and not a non-zero exit: a machine outside the ramp has
    // nothing wrong with it and nothing to do about it. The OS image is left
    // alone and packages, flatpaks and firmware still update, because a staged
    // rollout is about the image and nothing else.
    if !opts.firmware_only && !opts.force {
        if let Some(why) = crate::channel::rollout_hold() {
            print!("{why}");
            return finish_update(started, worst, &opts);
        }
    }

    if !opts.firmware_only {
        // What the machine is running BEFORE the pull, so the next run can tell
        // whether this one was rebooted into. Written first: a record written
        // after a successful upgrade would be missing for exactly the update
        // that crashed the machine, which is the one the gate exists for.
        match crate::channel::current_tag() {
            Ok(tag) => crate::channel::record_update(&tag),
            Err(e) => eprintln!("rime: the update health gate is not armed: {e}"),
        }
        // fsync off for the pull, restored when this drops — including on the
        // error paths below. See FsyncGuard for the measurements and the trade.
        let _fsync = if opts.keep_fsync {
            recover_stale_fsync();
            None
        } else {
            Some(FsyncGuard::disable())
        };
        // §22's boot-path migration, and it runs INSTEAD of the image update
        // when it does anything at all. Andre, 2026-09-20: "active machines
        // should automatically migrate with sudo rime install, not this
        // dumbass reinstall shit." This is where that happens — the normal
        // update path, no flag, nothing for the user to choose.
        //
        // Instead of, not as well as: the migration deploys the digest this
        // machine is ALREADY running, so it changes the boot path and nothing
        // else. Staging an ostree update in the same invocation would leave
        // one shutdown with two finalize paths to run, which is exactly the
        // kind of thing that turns a reboot into a recovery.
        //
        // A refusal is not a failure. A machine that must not migrate — Secure
        // Boot with an unsigned loader, an ESP too small to hold two
        // deployments — keeps booting GRUB and takes its update normally. The
        // engine prints why, in full, and that is deliberate: it is the reason
        // the machine is not getting a feature it was promised.
        if migrate_boot_path() {
            println!(
                "rime: the boot path was migrated. Reboot when you like; this update did not\n\
                 rime: change the OS image, and the next one will come through the new path."
            );
            return finish_update(started, worst, &opts);
        }

        // The rename. `bootc switch` stages the new name's image exactly as an
        // upgrade stages the old one's, and rewrites the origin, so every update
        // after this one is an ordinary `bootc upgrade` under the new name.
        let mut moved = false;
        if let Some(t) = &target {
            println!("apex: APEX is now Rime OS: moving this machine to {t}");  // rime-rename: keep
            match run("bootc", &["switch", t]) {
                Ok(0) => moved = true,
                Ok(code) => eprintln!(
                    "rime: could not move to {t} (bootc switch exited {code}); updating under {OLD_IMAGE} instead"
                ),
                Err(e) => eprintln!(
                    "rime: could not move to {t} ({e}); updating under {OLD_IMAGE} instead"
                ),
            }
            // The gate above verified the NEW name. Falling back deploys from
            // the old one, so that has to pass the same gate first; a refusal
            // here skips the image and still lets the rest of the update run.
            if !moved {
                if let Some(code) = trust_gate(opts.allow_unverified, None) {
                    return finish_update(started, worst.max(code), &opts);
                }
            }
        }
        if moved {
            return finish_update(started, worst, &opts);
        }

        // Deliberately NOT preceded by `bootc upgrade --check`: bootc already
        // no-ops when the booted image is current, and checking first would add
        // a second registry round-trip to the exact path we are trying to make
        // faster.
        //
        // The live-update engine stages with `--download-only`, verifies the
        // digest that was really staged, queues it, and activates what can be
        // activated without a restart. See `crate::live`.
        let live = crate::live::LiveOptions { allow_unverified: opts.allow_unverified, ..opts.live };
        match crate::live::update(live) {
            0 => {}
            code => {
                // bootc's own wording for a layered deployment names rpm-ostree
                // and offers `rpm-ostree reset`, which throws the user's
                // software away. Rime can do strictly better: keep the
                // packages, drop the layer.
                advise_on_layering();
                worst = worst.max(code);
            }
        }
        if opts.live.plan_only {
            return worst;
        }
    }

    finish_update(started, worst, &opts)
}

/// Update Flatpak applications as part of `rime update`.
///
/// Flatpak is where Rime puts sandboxed desktop apps, so leaving them out of
/// the one update command meant a machine could report itself fully up to date
/// while every graphical application on it was months stale — the user had no
/// way to know they were also supposed to run `flatpak update` by hand.
///
/// Never fatal: a flatpak that cannot reach Flathub must not fail an OS update.
fn flatpak_pass() -> i32 {
    if !Path::new("/usr/bin/flatpak").exists() {
        return 0;
    }
    match run(PKG_ENGINE, &["flatpak-upgrade"]) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("rime: Flatpak update skipped: {e}");
            0
        }
    }
}

/// Packages layered into the deployment with rpm-ostree, if any.
///
/// Text output rather than `--json`: this runs on the failure path of an update,
/// where the goal is one clear sentence, not a parser that can itself fail.
fn layered_packages() -> Option<String> {
    let out = capture("rpm-ostree", &["status"])?;
    out.lines()
        .map(str::trim)
        .find(|l| l.starts_with("LayeredPackages:"))
        .map(|l| l.trim_start_matches("LayeredPackages:").trim().to_string())
        .filter(|p| !p.is_empty())
}

/// Explain the one failure that stops a Rime machine updating, and the fix
/// that does not cost the user their software.
fn advise_on_layering() {
    if let Some(pkgs) = layered_packages() {
        eprintln!(
            "\nrime: this deployment has rpm-ostree layered packages, which block OS updates:\n\
             \x20       {pkgs}\n\
             \x20     Rime can move them into a system extension instead — same programs,\n\
             \x20     no layering, and updates start working again:\n\n\
             \x20       sudo rime pkg adopt\n"
        );
    }
}

/// The firmware half of `rime update`. Best-effort throughout: a machine may
/// have no fwupd, no LVFS-covered devices, or no network.
fn firmware_pass() -> i32 {
    // No `--force`. fwupd refreshes its metadata at most once every 24h by
    // design and reports "nothing to do" (2) inside that window; forcing it
    // re-downloaded the whole index on every invocation for no benefit.
    match run("fwupdmgr", &["refresh"]) {
        Ok(code) if fwupd_idle(code) => println!("rime: firmware metadata already current"),
        Ok(0) => {}
        Ok(code) => eprintln!("rime: fwupd refresh returned {code} — continuing anyway"),
        Err(e) => {
            eprintln!("rime: fwupd refresh skipped: {e}");
            return 0;
        }
    }

    // Ask before doing. `fwupdmgr update` on a machine with nothing to install
    // still enumerates every device and re-reads every plugin; `get-updates` is
    // the cheap question.
    match run("fwupdmgr", &["get-updates"]) {
        Ok(code) if fwupd_idle(code) => {
            println!("rime: no firmware updates for this machine");
            return 0;
        }
        Ok(0) => {}
        Ok(code) => {
            eprintln!("rime: fwupd get-updates returned {code} — skipping the firmware pass");
            return 0;
        }
        Err(e) => {
            eprintln!("rime: firmware update skipped: {e}");
            return 0;
        }
    }

    match run("fwupdmgr", &["update", "-y"]) {
        Ok(code) if fwupd_idle(code) => 0,
        Ok(code) => code,
        Err(e) => {
            eprintln!("rime: fwupd update skipped: {e}");
            0
        }
    }
}

/// `rime changelog` -> show the booted image and its OCI revision/version
/// labels (best-effort across bootc/rpm-ostree/skopeo).
pub fn changelog() -> i32 {
    if let Some(status) = capture("bootc", &["status"]) {
        println!("{status}");
        // Try to surface the image's git SHA / version labels if skopeo is
        // present and we can find the image ref.
        if let Some(image) = capture("bootc", &["status", "--format", "json"])
            .and_then(|j| extract_image_ref(&j))
        {
            println!("\nimage: {image}");
            if let Some(labels) = capture(
                "skopeo",
                &["inspect", "--format", "{{.Labels}}", &format!("docker://{image}")],
            ) {
                println!("labels: {labels}");
            }
        }
        return 0;
    }
    if let Some(status) = capture("rpm-ostree", &["status"]) {
        println!("{status}");
        return 0;
    }
    eprintln!("rime: neither bootc nor rpm-ostree available to read the changelog");
    1
}

/// Extremely small extractor for the `image` field of `bootc status --format
/// json` — avoids pulling a JSON crate for one field.
fn extract_image_ref(json: &str) -> Option<String> {
    let key = "\"image\"";
    let start = json.find(key)?;
    let rest = &json[start + key.len()..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let q1 = after.find('"')?;
    let after = &after[q1 + 1..];
    let q2 = after.find('"')?;
    let candidate = &after[..q2];
    if candidate.contains('/') || candidate.contains(':') {
        Some(candidate.to_string())
    } else {
        None
    }
}

/// Render the fingerprint as a human-readable block.
pub fn render_fingerprint(fp: &Fingerprint, sel: &Selection) -> String {
    let mut s = String::new();
    s.push_str("Machine\n");
    s.push_str(&format!("  vendor        : {}\n", fp.sys_vendor));
    s.push_str(&format!("  product       : {}\n", fp.product_name));
    s.push_str(&format!("  family        : {}\n", fp.product_family));
    s.push_str(&format!("  version       : {}\n", fp.product_version));
    s.push_str(&format!(
        "  chassis       : {} ({})\n",
        fp.chassis_type,
        if fp.is_laptop() { "laptop" } else { "desktop/other" }
    ));
    s.push_str("CPU\n");
    s.push_str(&format!("  vendor        : {}\n", fp.cpu.vendor.as_str()));
    s.push_str(&format!("  model         : {}\n", fp.cpu.model_name));
    s.push_str(&format!(
        "  topology      : {} cores / {} threads{}\n",
        fp.cpu.physical_cores,
        fp.cpu.logical_threads,
        if fp.cpu.hybrid { " (P/E hybrid)" } else { "" }
    ));
    s.push_str(&format!(
        "  scaling driver: {}\n",
        fp.cpu.scaling_driver.as_deref().unwrap_or("(unknown)")
    ));
    s.push_str("GPU\n");
    if fp.gpus.is_empty() {
        s.push_str("  (none detected)\n");
    }
    for g in &fp.gpus {
        s.push_str(&format!(
            "  {} [{}] @ {}\n",
            g.vendor.as_str(),
            g.pci_id(),
            g.pci_slot
        ));
    }
    if fp.intel_nvidia_hybrid_gpu() {
        s.push_str("  (Intel + NVIDIA hybrid / Optimus)\n");
    }
    s.push_str("Power supply\n");
    s.push_str(&format!("  AC present    : {}\n", fp.has_ac));
    s.push_str(&format!(
        "  batteries     : {}\n",
        if fp.batteries.is_empty() {
            "(none)".to_string()
        } else {
            fp.batteries.join(", ")
        }
    ));
    s.push_str("Profile (layered selection)\n");
    s.push_str(&format!("  generic       : {}\n", sel.generic));
    s.push_str(&format!(
        "  class         : {}\n",
        if sel.class_or_empty().is_empty() {
            "(none)"
        } else {
            sel.class_or_empty()
        }
    ));
    s.push_str(&format!(
        "  device        : {}\n",
        if sel.device_or_empty().is_empty() {
            "(none)"
        } else {
            sel.device_or_empty()
        }
    ));
    s.push_str(&format!("  active        : {}\n", sel.active));
    s
}

/// Render the per-tier dry-run plan for a profile (what the daemon *would*
/// apply). No hardware is touched.
pub fn render_tier_plans(profile: &Profile) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "Dry-run tier plans for profile '{}' (no hardware touched):\n",
        profile.id
    ));
    for tier in Tier::ALL {
        s.push_str(&format!("  {} [{}]\n", tier.label(), tier.as_str()));
        let plan = profile.plan_tier(tier);
        if plan.is_empty() {
            s.push_str("    (no actions)\n");
        }
        for a in plan {
            s.push_str(&format!("    - {}\n", a.describe()));
        }
    }
    // Charge thresholds are resolved against the batteries this machine
    // actually has, so the dry-run view reports discovery too — including
    // "unsupported", which is the honest answer on most hardware.
    if let Some((start, stop)) = profile.charge_window() {
        s.push_str("  charge defaults\n");
        let inv = rimed_core::BatteryInventory::detect();
        let plan = inv.plan_thresholds(start, stop);
        if plan.is_empty() {
            s.push_str(&format!(
                "    - wants {start}-{stop}, but no battery here accepts thresholds ({})\n",
                inv.summary()
            ));
        }
        for a in plan {
            s.push_str(&format!("    - {}\n", a.describe()));
        }
    }
    s.push_str(&format!(
        "  auto-switch defaults: AC -> {}, battery -> {}\n",
        profile.defaults.ac.as_str(),
        profile.defaults.battery.as_str()
    ));
    s
}

/// Local (daemon-less) read-only view: fingerprint + selection + resolved
/// profile handle.
pub struct LocalView {
    pub fingerprint: Fingerprint,
    pub selection: Selection,
    pub set: ProfileSet,
}

impl LocalView {
    pub fn detect() -> LocalView {
        let fingerprint = Fingerprint::detect();
        let set = ProfileSet::load(Some(Path::new(rimed_core::PROFILE_DIR)))
            .unwrap_or_else(|_| ProfileSet::builtin());
        let selection = rimed_core::select(&fingerprint, &set);
        LocalView {
            fingerprint,
            selection,
            set,
        }
    }

    /// The resolved profile. Falls back to the generic layer (which
    /// `ProfileSet` always retains) rather than panicking, so a broken override
    /// directory downgrades the CLI's answer instead of aborting it.
    pub fn active_profile(&self) -> &Profile {
        self.set
            .get(&self.selection.active)
            .or_else(|| self.set.get(&self.selection.generic))
            .expect("profile set always retains a generic layer")
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────
// The root gate and the fwupd exit-code reading are both places where being
// subtly wrong is invisible: one would let a privileged verb through (or block
// an unprivileged one), the other would make a completely successful update
// report failure. Both are pure functions precisely so they can be pinned here.
#[cfg(test)]
mod rename_tests {
    use super::renamed_reference;

    #[test]
    fn a_tag_of_the_old_name_moves_to_the_same_tag_of_the_new_one() {
        for tag in ["apex", "rime", "daily", "gaming-mesa", "gaming-nvidia", "edge"] {  // rime-rename: keep (apex is a tag machines track)
            assert_eq!(
                renamed_reference(&format!("ghcr.io/andrenijman/apex-os:{tag}")).as_deref(),  // rime-rename: keep
                Some(format!("ghcr.io/andrenijman/rime-os:{tag}").as_str())
            );
        }
    }

    #[test]
    fn a_digest_pin_a_fork_and_the_new_name_stay_where_they_are() {
        assert_eq!(renamed_reference("ghcr.io/andrenijman/apex-os@sha256:abc"), None);  // rime-rename: keep
        assert_eq!(renamed_reference("ghcr.io/andrenijman/apex-os:daily@sha256:abc"), None);  // rime-rename: keep
        assert_eq!(renamed_reference("ghcr.io/someone/rime-os:daily"), None);
        assert_eq!(renamed_reference("ghcr.io/andrenijman/rime-os:daily"), None);
        assert_eq!(renamed_reference("ghcr.io/andrenijman/apex-os-core:latest"), None);  // rime-rename: keep
        assert_eq!(renamed_reference("ghcr.io/andrenijman/apex-os"), None);  // rime-rename: keep
        assert_eq!(renamed_reference("ghcr.io/andrenijman/apex-os:"), None);  // rime-rename: keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "Name:\trime\nUmask:\t0022\nState:\tR (running)\n\
                          Uid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\n";

    #[test]
    fn reads_the_effective_uid_not_the_real_one() {
        // real 1000, effective 0 — a setuid-style split. The effective id is
        // what the kernel enforces, so it is what we must read.
        let s = "Name:\trime\nUid:\t1000\t0\t0\t1000\n";
        assert_eq!(parse_effective_uid(s), Some(0));
        assert_eq!(parse_effective_uid(STATUS), Some(1000));
    }

    #[test]
    fn missing_or_malformed_status_is_not_root() {
        assert_eq!(parse_effective_uid(""), None);
        assert_eq!(parse_effective_uid("Name:\trime\n"), None);
        assert_eq!(parse_effective_uid("Uid:\n"), None);
        assert_eq!(parse_effective_uid("Uid:\t1000\n"), None);
        assert_eq!(parse_effective_uid("Uid:\tx\ty\n"), None);
        // Every one of these must fail CLOSED: require_root treats anything
        // other than Some(0) as "not root".
    }

    #[test]
    fn the_hint_echoes_the_whole_invocation() {
        let argv: Vec<String> = ["rime", "update", "--check", "--skip-firmware"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            sudo_reinvocation(&argv),
            "sudo rime update --check --skip-firmware"
        );
        // …including the no-argument case, where it must not gain a trailing space.
        assert_eq!(sudo_reinvocation(&["rime".to_string()]), "sudo rime");
    }

    #[test]
    fn the_refusal_names_the_verb_and_the_fix() {
        let argv: Vec<String> = ["rime", "update"].iter().map(|s| s.to_string()).collect();
        let msg = root_required_message("update", &argv);
        assert!(msg.contains("'update'"));
        assert!(msg.contains("sudo rime update"));
    }

    #[test]
    fn fwupd_nothing_to_do_is_success_not_failure() {
        // This is the regression that matters: 2 and 3 are the ORDINARY
        // outcomes on an up-to-date laptop. Reading them as failure would make
        // `rime update` exit non-zero on its most common path.
        assert!(fwupd_idle(FWUPD_NOTHING_TO_DO));
        assert!(fwupd_idle(FWUPD_NOT_FOUND));
        assert!(!fwupd_idle(0));
        assert!(!fwupd_idle(1));
    }
}
