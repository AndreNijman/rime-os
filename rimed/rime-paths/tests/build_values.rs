//! What each constant holds in THIS build.
//!
//! The bootc image must get exactly the literals the code used before the path
//! module existed, so they are written out again here, by hand, and compared
//! with the defaults in `src/lib.rs`: editing a default there without meaning
//! to fails this test in every build.

use rime_paths::ALL;

/// The literals as they stood in the source before `rime-paths` (rime-os
/// c18800d15). Changing one changes what a bootc machine runs.
const BOOTC: &[(&str, &str)] = &[
    ("BWRAP", "/usr/bin/bwrap"),
    ("SYSTEMD_RUN", "/usr/bin/systemd-run"),
    ("SYSTEMCTL", "/usr/bin/systemctl"),
    ("BOOTCTL", "/usr/bin/bootctl"),
    ("CURL", "/usr/bin/curl"),
    ("OPENSSL", "/usr/bin/openssl"),
    ("GIT", "/usr/bin/git"),
    ("SSH", "/usr/bin/ssh"),
    ("SKOPEO", "/usr/bin/skopeo"),
    ("RPM_OSTREE", "/usr/bin/rpm-ostree"),
    ("FWUPDMGR", "/usr/bin/fwupdmgr"),
    ("FLATPAK", "/usr/bin/flatpak"),
    ("SMARTCTL", "/usr/sbin/smartctl"),
    ("WIPEFS", "/usr/sbin/wipefs"),
    ("SCXCTL", "/usr/sbin/scxctl"),
    ("SCXCTL_ALT", "/usr/bin/scxctl"),
    ("RIME", "/usr/bin/rime"),
    ("RIME_AID", "/usr/bin/rime-aid"),
    ("RIME_PKG", "/usr/libexec/rime-pkg"),
    ("RIME_ENV", "/usr/libexec/rime-env"),
    ("RIME_DISPOSABLE", "/usr/libexec/rime-disposable"),
    ("RIME_USER", "/usr/libexec/rime-user"),
    ("RIME_VM", "/usr/libexec/rime-vm"),
    ("RIME_BROWSER", "/usr/libexec/rime-browser"),
    ("RIME_PLUGIN", "/usr/libexec/rime-plugin"),
    ("RIME_FIREWALL", "/usr/libexec/rime-firewall"),
    ("RIME_DEVICES", "/usr/libexec/rime-devices"),
    ("RIME_BOOT_MIGRATE", "/usr/libexec/rime-boot-migrate"),
    ("RIME_LIVE_SESSION", "/usr/libexec/rime-live-session"),
    ("RIME_PROJECT_WINDOWS", "/usr/libexec/rime-project-windows"),
    ("RIME_MUX", "/usr/libexec/rime-mux"),
    ("RIME_SESSION_SELECT", "/usr/libexec/rime-session-select"),
    ("RIME_SHELL_FIRSTRUN", "/usr/libexec/rime-shell-firstrun"),
    ("RIME_TOOLS_DIR", "/usr/libexec/rime/tools"),
    ("SYSPROFILES_DIR", "/usr/share/rimeos/sysprofiles"),
    ("AI_CATALOGUE", "/usr/share/rimeos/ai/catalogue.toml"),
    ("TRUST_FULCIO_ROOT", "/usr/share/rime-os/trust/fulcio-root.pem"),
    ("TRUST_EXPECTED_SIGNER", "/usr/share/rime-os/trust/expected-signer"),
    ("TRUST_EXPECTED_ROLLOUT_SIGNER", "/usr/share/rime-os/trust/expected-rollout-signer"),
    ("TRUST_EXPECTED_ISSUER", "/usr/share/rime-os/trust/expected-issuer"),
    ("TRUST_ENFORCEMENT", "/usr/share/rime-os/trust/enforcement.conf"),
    ("SHELL_DIR", "/usr/share/rime-shell"),
    ("SHELL_QML", "/usr/share/rime-shell/shell.qml"),
    ("SYSTEM_BIN_DIR", "/usr/bin"),
    ("LOCAL_BIN_DIR", "/usr/local/bin"),
    ("OS_RELEASE_FALLBACK", "/usr/lib/os-release"),
    ("RESCUE_TARGET_UNIT", "/usr/lib/systemd/system/rescue.target"),
    ("LIBCUDA", "/usr/lib64/libcuda.so.1"),
    ("VULKAN_ICD_DIR", "/usr/share/vulkan/icd.d"),
    ("SAFE_PATH", "/usr/local/bin:/usr/bin:/bin"),
    ("MINIMAL_PATH", "/usr/bin:/bin"),
    ("AID_SANDBOX_PATH", "/usr/bin:/usr/sbin"),
];

#[test]
fn defaults_are_the_bootc_literals() {
    assert_eq!(ALL.len(), BOOTC.len(), "a path was added or removed without updating BOOTC here");
    for (name, _, default) in ALL {
        let want = BOOTC
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("{name} is missing from BOOTC"))
            .1;
        assert_eq!(*default, want, "{name}'s bootc default changed");
    }
}

/// A build that sets no `RIME_PATH_*` (the bootc image) gets every default.
/// A build that promises to set them all (the Nix derivation sets
/// `RIME_PATHS_EXPECT_ALL_SET=1`) must not have left a single one at a `/usr`
/// default: a NixOS machine has no `/usr/bin/bwrap`, and a sandbox pointed at a
/// missing file refuses to start rather than running unconfined only if the
/// code was written that way everywhere — not something to rely on.
#[test]
fn this_build_sets_none_or_all() {
    if option_env!("RIME_PATHS_EXPECT_ALL_SET").is_some() {
        let left: Vec<_> = ALL.iter().filter(|(_, v, d)| v == d).map(|(n, _, _)| *n).collect();
        assert!(left.is_empty(), "left at the bootc default in a build that sets every path: {left:?}");
        for (name, value, _) in ALL {
            assert!(!value.split(':').any(|p| p.starts_with("/usr/")), "{name} = {value} is under /usr");
        }
    } else {
        for (name, value, default) in ALL {
            assert_eq!(value, default, "{name} differs from the bootc default in a bootc build");
        }
    }
}
