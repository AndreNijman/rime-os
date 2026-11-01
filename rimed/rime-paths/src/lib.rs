//! Every fixed absolute path Rime's binaries run or read, in one place.
//!
//! # Why this exists
//!
//! Rime runs a few programs by absolute path ON PURPOSE. `bwrap` is the agent
//! sandbox's security boundary, the `/usr/libexec/rime-*` engines do privileged
//! work, and `curl` carries brokered credentials. Resolving any of them through
//! `PATH` would let a shadowing binary in a directory the user controls decide
//! what "confined" or "the package engine" means (see `sandbox::bwrap_path` and
//! the engine constants in `rime/src/ops.rs`). So they stay constants.
//!
//! What differs is where a given operating system keeps them. On the bootc
//! image they live under `/usr`; on NixOS there is no `/usr/bin` or
//! `/usr/libexec` at all and they are store paths or paths under
//! `/run/current-system`. This crate is the only place that difference exists.
//!
//! # How a value is chosen
//!
//! At COMPILE time. Each constant reads the environment variable
//! `RIME_PATH_<NAME>` with `option_env!`, and when it is unset takes the bootc
//! default written next to it. Nothing here reads the environment at run
//! time and nothing here searches `PATH`: a built binary holds one answer and
//! cannot be talked out of it.
//!
//! * The bootc image (`Containerfile.base`) sets none of the variables, so every
//!   constant is exactly the literal the code used before this crate existed.
//!   `tests/build_values.rs` pins each one.
//! * The Nix derivation (`nix/pkgs/rimed.nix`) sets every one of them, and sets
//!   `RIME_PATHS_EXPECT_ALL_SET=1`, under which `tests/build_values.rs` fails if
//!   any constant was left at its bootc default or under /usr.
//!
//! Every absolute `/usr/...` literal elsewhere in the workspace's non-test code
//! must come from here, apart from the bootc-image tables listed in
//! `tests/no_stray_literals.rs` with their reasons. That test fails on a new one.

/// Declares each path: a `pub const` chosen at compile time, plus [`ALL`].
macro_rules! fixed_paths {
    ($( $(#[$meta:meta])* $name:ident = $default:literal; )*) => {
        $(
            $(#[$meta])*
            #[doc = concat!("\n\nbootc default: `", $default, "`. Build-time override: `RIME_PATH_", stringify!($name), "`.")]
            pub const $name: &str = match option_env!(concat!("RIME_PATH_", stringify!($name))) {
                Some(p) => p,
                None => $default,
            };
            const _: () = assert!(is_fixed(&$name), concat!("RIME_PATH_", stringify!($name), " must be absolute"));
        )*

        /// Every path: `(name, value in this build, bootc default)`.
        pub const ALL: &[(&str, &str, &str)] = &[ $( (stringify!($name), $name, $default), )* ];
    };
}

/// A fixed path is absolute and not empty. A `PATH`-style list (see the
/// `*_PATH` constants) is a `:`-separated list of such paths, so the same rule
/// applies to its first entry. Checked at compile time, so a build that sets a
/// variable to `bwrap` instead of `/nix/store/.../bin/bwrap` does not build.
const fn is_fixed(p: &&str) -> bool {
    let b = p.as_bytes();
    !b.is_empty() && b[0] == b'/'
}

fixed_paths! {
    // ── host programs run by absolute path ────────────────────────────────────

    /// bubblewrap: the agent sandbox and the rime-aid backend sandbox.
    BWRAP = "/usr/bin/bwrap";
    /// systemd-run: an agent session's resource budget (`rime-agentd/src/budget.rs`).
    SYSTEMD_RUN = "/usr/bin/systemd-run";
    /// systemctl, where a caller runs it by absolute path.
    SYSTEMCTL = "/usr/bin/systemctl";
    /// bootctl: `rime boot`.
    BOOTCTL = "/usr/bin/bootctl";
    /// curl: the secret broker's HTTP and the OAuth device flow.
    CURL = "/usr/bin/curl";
    /// openssl: signature checks in `rime trust` / `rime verify`.
    OPENSSL = "/usr/bin/openssl";
    /// git: the git shim's real git.
    GIT = "/usr/bin/git";
    /// ssh: the backup ssh target.
    SSH = "/usr/bin/ssh";
    /// skopeo: OCI signature fetches (bootc trust gate).
    SKOPEO = "/usr/bin/skopeo";
    /// rpm-ostree: the booted digest (bootc trust gate).
    RPM_OSTREE = "/usr/bin/rpm-ostree";
    /// fwupdmgr: `rime firmware`.
    FWUPDMGR = "/usr/bin/fwupdmgr";
    /// flatpak: whether `rime update` has a Flatpak pass to run.
    FLATPAK = "/usr/bin/flatpak";
    /// smartctl: `rime storage`.
    SMARTCTL = "/usr/sbin/smartctl";
    /// wipefs: `rime storage`.
    WIPEFS = "/usr/sbin/wipefs";
    /// scxctl, first place looked (scx-tools installs into /usr/sbin).
    SCXCTL = "/usr/sbin/scxctl";
    /// scxctl, second place looked.
    SCXCTL_ALT = "/usr/bin/scxctl";

    // ── Rime's own programs ────────────────────────────────────────────────────

    /// The `rime` CLI.
    RIME = "/usr/bin/rime";
    /// The local inference daemon.
    RIME_AID = "/usr/bin/rime-aid";

    // ── Rime's engines (bootc: /usr/libexec) ──────────────────────────────────

    /// The package engine.
    RIME_PKG = "/usr/libexec/rime-pkg";
    /// The capsule engine.
    RIME_ENV = "/usr/libexec/rime-env";
    /// The disposable-environment engine.
    RIME_DISPOSABLE = "/usr/libexec/rime-disposable";
    /// The account engine.
    RIME_USER = "/usr/libexec/rime-user";
    /// The VM engine.
    RIME_VM = "/usr/libexec/rime-vm";
    /// The browser engine.
    RIME_BROWSER = "/usr/libexec/rime-browser";
    /// The plugin engine.
    RIME_PLUGIN = "/usr/libexec/rime-plugin";
    /// The firewall engine.
    RIME_FIREWALL = "/usr/libexec/rime-firewall";
    /// The devices engine.
    RIME_DEVICES = "/usr/libexec/rime-devices";
    /// The GRUB to systemd-boot migration engine (bootc only).
    RIME_BOOT_MIGRATE = "/usr/libexec/rime-boot-migrate";
    /// The per-session live-update helper (bootc only).
    RIME_LIVE_SESSION = "/usr/libexec/rime-live-session";
    /// The compositor adapter for project windows.
    RIME_PROJECT_WINDOWS = "/usr/libexec/rime-project-windows";
    /// The terminal multiplexer adapter.
    RIME_MUX = "/usr/libexec/rime-mux";
    /// The session selector.
    RIME_SESSION_SELECT = "/usr/libexec/rime-session-select";
    /// The per-user Shell provisioner.
    RIME_SHELL_FIRSTRUN = "/usr/libexec/rime-shell-firstrun";
    /// The directory of tool shims put on an agent session's PATH.
    RIME_TOOLS_DIR = "/usr/libexec/rime/tools";

    // ── data Rime installs ────────────────────────────────────────────────────

    /// System profile overrides (the profiles themselves are compiled in).
    SYSPROFILES_DIR = "/usr/share/rimeos/sysprofiles";
    /// The local-model catalogue.
    AI_CATALOGUE = "/usr/share/rimeos/ai/catalogue.toml";
    /// The pinned Fulcio root.
    TRUST_FULCIO_ROOT = "/usr/share/rime-os/trust/fulcio-root.pem";
    /// Image signer override.
    TRUST_EXPECTED_SIGNER = "/usr/share/rime-os/trust/expected-signer";
    /// Rollout-document signer override.
    TRUST_EXPECTED_ROLLOUT_SIGNER = "/usr/share/rime-os/trust/expected-rollout-signer";
    /// Certificate issuer override.
    TRUST_EXPECTED_ISSUER = "/usr/share/rime-os/trust/expected-issuer";
    /// Trust enforcement defaults.
    TRUST_ENFORCEMENT = "/usr/share/rime-os/trust/enforcement.conf";
    /// Rime Shell's QML tree.
    SHELL_DIR = "/usr/share/rime-shell";
    /// Rime Shell's entry point (`SHELL_DIR` + `/shell.qml`).
    SHELL_QML = "/usr/share/rime-shell/shell.qml";

    // ── host layout ───────────────────────────────────────────────────────────

    /// Where the system's programs are (compositors, a system-installed
    /// inference runtime).
    SYSTEM_BIN_DIR = "/usr/bin";
    /// A second directory a tool may be installed into.
    LOCAL_BIN_DIR = "/usr/local/bin";
    /// os-release, when /etc/os-release is missing.
    OS_RELEASE_FALLBACK = "/usr/lib/os-release";
    /// Whether the rescue target exists.
    RESCUE_TARGET_UNIT = "/usr/lib/systemd/system/rescue.target";
    /// NVIDIA's CUDA driver library.
    LIBCUDA = "/usr/lib64/libcuda.so.1";
    /// Vulkan ICD manifests.
    VULKAN_ICD_DIR = "/usr/share/vulkan/icd.d";

    // ── PATH for children whose environment is cleared ─────────────────────────
    //
    // On Unix, `Command` looks a bare program name up in the CHILD's PATH once
    // one is set, so these decide which `git` or `timeout` a cleared child runs.

    /// The secret broker's children (`timeout git ...`), and an agent session's
    /// PATH when the daemon itself has none.
    SAFE_PATH = "/usr/local/bin:/usr/bin:/bin";
    /// An MCP sidecar's PATH when the caller has none.
    MINIMAL_PATH = "/usr/bin:/bin";
    /// The rime-aid backend's PATH inside its sandbox.
    AID_SANDBOX_PATH = "/usr/bin:/usr/sbin";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        let mut names: Vec<&str> = ALL.iter().map(|(n, _, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len());
    }

    #[test]
    fn every_value_is_absolute() {
        for (name, value, _) in ALL {
            for part in value.split(':') {
                assert!(part.starts_with('/'), "{name} = {value:?}: {part:?} is not absolute");
            }
        }
    }
}
