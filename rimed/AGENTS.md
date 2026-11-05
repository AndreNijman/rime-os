# rimed, CLI, and policy review

- `rimed` is a privileged system policy daemon. Validate every caller-controlled value before filesystem, process, sysfs, D-Bus, package, boot, fan, battery, or power operations. Authorization belongs in D-Bus/polkit policy as well as the CLI UX.
- Preserve the frozen `org.rimeos.Rimed1` interfaces and documented semantics. Flag changed names, object paths, signatures, units, ranges, defaults, error behavior, or authorization even when Rust still compiles.
- Hardware matching must be deterministic and conservative. Generic profiles are safe fallback floors. Model-specific fan, power, CPU, GPU, EC, charge, IRQ, or kernel tuning must activate only on positively identified hardware.
- Power tiers must be reversible and idempotent. Save/restore or recompute all changed state; failures midway must not leave a mixed unsafe profile. Battery operation must never inherit AC-only limits accidentally.
- Game mode is orchestration, not permanent mutation. Processes/scopes, CPU sets, GPU settings, services, inhibitors, and fan state must be cleaned up after normal exit, crash, daemon restart, and cancellation.
- Package management must preserve bootc compatibility. Never introduce rpm-ostree layering. System extensions must verify RPM signatures, reject core ABI/kernel replacements, preserve user-modified `/etc`, label for SELinux (bootc; the NixOS store carries no labels and has no SELinux), rebuild safely across OS compatibility changes, and retain independent rollback.
- Commands that report success must verify the requested state. Dry-run must perform no writes. Read-only commands should remain usable without root; mutating commands must fail clearly without authorization.
- Avoid panics in daemon paths. Surface actionable typed/contextual errors without secrets. Bound retries and loops, handle missing sysfs/device nodes, and degrade unsupported features rather than taking down the daemon.
- Run `cargo clippy --all-targets --locked -- -D warnings` and `cargo test --locked`. Add focused tests for parsing, profile selection, range boundaries, rollback/cleanup, failed writes, and newly supported hardware.
