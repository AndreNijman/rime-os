#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-env-devices-live.sh — the capsule device profiles, against the
#  hardware that is actually in this machine.
#
#  ── Why this file exists ────────────────────────────────────────────────────
#  tests/test-rime-env.sh asserts the exact distrobox argv each `--gpu` profile
#  produces, and it does so with a stubbed container manager because
#  `rime env create` is the one verb that pulls hundreds of megabytes and makes
#  a container. That is the right design and it is deliberately kept: nothing
#  here changes it.
#
#  But it leaves one thing unanswered, and §BASE-005 recorded it as the
#  outstanding qualification for "GPU/device profiles work where supported":
#  the AMD and hw profiles were argv-PINNED and never hardware-verified. A
#  pinned string proves the engine emits what its author intended. It does not
#  prove those flags reach the device, and the whole point of the `amd` and
#  `hw` profiles is device access.
#
#  So this suite asks the SHIPPED engine what a profile adds — it sources
#  rime-env and calls `gpu_flags`, so a change to the profile changes this test
#  rather than sliding past it — and then hands exactly those flags to real
#  rootless podman on an image that is already on this machine, and reads back
#  what is reachable inside.
#
#  ── What it will not do ─────────────────────────────────────────────────────
#    * it never calls `rime env create`, so no distrobox container, no capsule
#      record, and nothing in ~/.local/share/rime/env;
#    * it never pulls: an image that is not already local is a SKIP;
#    * every container is `--rm` and runs one read-only probe;
#    * it opens no window and asks for no password.
#
#  A machine without the hardware skips with status 0, which is what CI will
#  normally do — the value of this file is on a machine that has the device.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# Deliberately +e. CI invokes a suite as `bash -e {0}`, and under -e the first
# assignment from a failing command ends the run silently — the same reason
# test-rime-env.sh and both labwc suites do this.
set +e

cd "$(dirname "$0")" || exit 2
ROOT="$(cd .. && pwd)"
ENGINE="$ROOT/files/system/libexec/rime-env"
[ -f "$ENGINE" ] || { echo "FATAL: cannot find $ENGINE" >&2; exit 2; }

pass=0; fail=0; skip=0
# How many containers this suite actually started. `inside` and the hw runs
# bump it in the CURRENT shell (never inside a command substitution), so the
# teardown section below can tell "nothing leaked" from "nothing ran".
started_containers=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s%s\n' "$1" "${2:+  — $2}"; fail=$((fail + 1)); }
skp() { printf 'SKIP  %s%s\n' "$1" "${2:+  — $2}"; skip=$((skip + 1)); }
section() { printf '\n── %s ──\n' "$1"; }
finish() {
    printf '\nrime-env-devices: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    [ "$fail" -eq 0 ]
}

# ── what the engine says a profile adds ─────────────────────────────────────
# Sourced, not re-implemented. `gpu_flags` prints one argument per line
# precisely so a caller can read it into an array, and that is what the engine
# does with it too.
section "the shipped engine's own device profiles"
# `set --` BEFORE sourcing, exactly as test-rime-env.sh's `call` does it and
# for the same reason: the engine ends in `main "$@"`, so a sourced copy that
# can still see positional arguments runs a COMMAND. Left in place, `gpu_flags
# amd` sourced the engine as `rime-env amd`, which is an unknown command, and
# `die` took the subshell with it — every profile then read as "adds nothing"
# and the `none` assertion passed for that reason rather than its own.
flags_for() {
    local profile="$1"
    # shellcheck disable=SC1090  # the shipped engine, resolved from $0 above; the
    # point of this suite is that it reads THAT file and not a copy of its rules
    ( set +u; set --; source "$ENGINE" >/dev/null 2>&1; set +e; gpu_flags "$profile" )
}
engine_detect_gpu() {
    # shellcheck disable=SC1090  # same file, same reason
    ( set +u; set --; source "$ENGINE" >/dev/null 2>&1; set +e; detect_gpu )
}

# The shape matters as much as the content. gpu_flags emits distrobox's
# `--additional-flags` and then ONE argument holding the podman flags, so this
# suite must take the second line and not guess. If that shape ever changes,
# every assertion below would quietly have no devices to pass and would pass
# for the wrong reason — so the shape is asserted first and a parse that finds
# no --device is a FAILURE, never a skip.
podman_flags_of() {
    local profile="$1" out first rest
    out="$(flags_for "$profile")" || return 1
    first="$(sed -n 1p <<<"$out")"
    rest="$(sed -n 2p <<<"$out")"
    [ "$first" = "--additional-flags" ] || return 2
    printf '%s' "$rest"
}

for profile in amd hw; do
    if pf="$(podman_flags_of "$profile")"; then
        case "$pf" in
            *--device*) ok "the '$profile' profile hands podman at least one --device" ;;
            *) bad "the '$profile' profile hands podman a --device" "got: $pf" ;;
        esac
    else
        bad "the '$profile' profile is shaped as --additional-flags plus one argument" \
            "gpu_flags '$profile' printed: $(flags_for "$profile" | tr '\n' '|')"
    fi
done
# `none` must add nothing at all: the default has to stay the one that holds no
# device open, or a capsule stops the host from suspending.
if [ -z "$(flags_for none)" ]; then
    ok "the 'none' profile adds nothing, so the default holds no device open"
else
    bad "the 'none' profile adds nothing" "got: $(flags_for none | tr '\n' '|')"
fi

# ── prerequisites, each a skip and never a silent pass ──────────────────────
section "what this machine can actually verify"
command -v podman >/dev/null 2>&1 || {
    skp "podman is installed" "no podman; the profiles cannot be exercised here"
    finish; exit $?
}
ok "podman is installed"

# An image that is already here. Pulling would make a test that needs the
# network and a gigabyte, on a suite whose whole point is a device node.
IMAGE=""
for cand in registry.fedoraproject.org/fedora:45 registry.fedoraproject.org/fedora:latest \
            docker.io/library/fedora:latest docker.io/library/debian:stable; do
    if podman image exists "$cand" 2>/dev/null; then IMAGE="$cand"; break; fi
done
if [ -z "$IMAGE" ]; then
    skp "an image is already on this machine" "nothing local to probe with, and this suite never pulls"
    finish; exit $?
fi
ok "an image is already on this machine ($IMAGE)"

# The probe that runs inside. Reads only: presence, and whether the node can be
# OPENED, which is the question a profile exists to answer. `dd … count=0`
# opens and reads nothing.
read -r -d '' PROBE <<'PROBE_EOF'
for d in "$@"; do
    if [ -e "$d" ]; then
        if dd if="$d" of=/dev/null bs=1 count=0 2>/dev/null; then echo "OPEN $d"
        else echo "REFUSED $d"; fi
    else
        echo "ABSENT $d"
    fi
done
PROBE_EOF

# Every container this suite starts is NAMED, and the name carries this run's
# pid. That is the only thing that makes the teardown assertion at the bottom
# capable of failing: podman's default names are random, so a filter that does
# not know what to look for finds nothing whether or not `--rm` did its job.
PROBE_NAME="rimedev-probe-$$"

inside() {
    local -a extra=()
    while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do extra+=("$1"); shift; done
    shift
    started_containers=$((started_containers + 1))
    podman run --rm --name "$PROBE_NAME-$RANDOM" "${extra[@]}" \
        "$IMAGE" bash -c "$PROBE" -- "$@" 2>&1
}

# ── the amd profile, against the GPU in this machine ────────────────────────
section "the amd profile"
AMD_NODES=(/dev/kfd /dev/dri/renderD128)
if [ "$(engine_detect_gpu)" = amd ] && [ -e /dev/kfd ]; then
    ok "the engine reads this machine's hardware as 'amd', from /dev/kfd"

    read -r -a AMD_FLAGS <<<"$(podman_flags_of amd)"
    if [ "${#AMD_FLAGS[@]}" -eq 0 ]; then
        bad "the amd profile's flags parsed into something to pass" "empty"
    else
        ok "the amd profile's flags parsed into ${#AMD_FLAGS[*]} arguments to pass"
        out="$(inside "${AMD_FLAGS[@]}" -- "${AMD_NODES[@]}")"
        for n in "${AMD_NODES[@]}"; do
            if grep -qx "OPEN $n" <<<"$out"; then
                ok "inside a real rootless container, $n is present AND opens"
            else
                bad "inside a real rootless container, $n opens" \
                    "$(grep -F "$n" <<<"$out" | head -1)"
            fi
        done

        # The negative control, and this suite is worthless without it: the
        # same image with NO flags must not have the nodes. Otherwise every
        # assertion above could be passing because podman hands every container
        # a /dev/dri regardless of what the profile asked for.
        outn="$(inside -- "${AMD_NODES[@]}")"
        for n in "${AMD_NODES[@]}"; do
            if grep -qx "ABSENT $n" <<<"$outn"; then
                ok "…and without the profile's flags, $n is not there at all"
            else
                bad "without the profile's flags, $n is absent" \
                    "$(grep -F "$n" <<<"$outn" | head -1)"
            fi
        done

        # `--group-add keep-groups` is in the profile for a stated reason: that
        # /dev/kfd needs the `render` group and rootless podman drops the
        # user's supplementary groups. MEASURED here rather than asserted,
        # because on this machine it changes nothing — amdgpu leaves /dev/kfd
        # mode 0666 and the seat's card node carries a per-user ACL, so no
        # supplementary group is what grants access. The flag is harmless and
        # would be load-bearing on a machine whose ROCm packaging installs the
        # udev rule that makes /dev/kfd root:render 0660. Reported, so the next
        # reader does not take this suite as proof of the group half.
        nogroups=()
        for f in "${AMD_FLAGS[@]}"; do
            [ "$f" = "--group-add" ] || [ "$f" = "keep-groups" ] || nogroups+=("$f")
        done
        outk="$(inside "${nogroups[@]}" -- /dev/kfd)"
        if grep -qx "OPEN /dev/kfd" <<<"$outk"; then
            skp "keep-groups is what grants /dev/kfd" \
                "/dev/kfd opens WITHOUT it here (host mode $(stat -c %a /dev/kfd)), so the group half is unverifiable on this machine"
        else
            ok "keep-groups is what grants /dev/kfd: without it the node is $(grep -F /dev/kfd <<<"$outk")"
        fi
    fi
else
    skp "the amd profile against real hardware" \
        "the engine reads this machine as '$(engine_detect_gpu)'; no /dev/kfd to reach"
fi

# ── the hw profile ──────────────────────────────────────────────────────────
section "the hw profile"
if [ -d /dev/bus/usb ]; then
    read -r -a HW_FLAGS <<<"$(podman_flags_of hw)"
    if [ "${#HW_FLAGS[@]}" -eq 0 ]; then
        bad "the hw profile's flags parsed into something to pass" "empty"
    else
        # A directory, not a character device, so the question is whether the
        # tree arrives — a programmer or JTAG probe is a node underneath it.
        #
        # Counted HERE and not inside the substitutions below: `$(…)` is a
        # subshell, so an increment written in there is discarded and the
        # teardown section would read zero and skip.
        started_containers=$((started_containers + 2))
        out="$(podman run --rm --name "$PROBE_NAME-$RANDOM" "${HW_FLAGS[@]}" "$IMAGE" \
                 bash -c 'if [ -d /dev/bus/usb ] && [ -n "$(ls -A /dev/bus/usb 2>/dev/null)" ]; then echo HAVE; else echo NOPE; fi' 2>&1)"
        if grep -qx HAVE <<<"$out"; then
            ok "inside a real rootless container, /dev/bus/usb arrives with buses under it"
        else
            bad "inside a real rootless container, /dev/bus/usb arrives" "$out"
        fi
        outn="$(podman run --rm --name "$PROBE_NAME-$RANDOM" "$IMAGE" \
                 bash -c 'if [ -d /dev/bus/usb ]; then echo HAVE; else echo NOPE; fi' 2>&1)"
        if grep -qx NOPE <<<"$outn"; then
            ok "…and without the profile's flags there is no /dev/bus/usb"
        else
            bad "without the profile's flags there is no /dev/bus/usb" "$outn"
        fi
    fi
else
    skp "the hw profile against real hardware" "this machine has no /dev/bus/usb"
fi

# ── nvidia stays unverified here, and says so ───────────────────────────────
section "what this machine cannot verify"
if [ -e /dev/nvidiactl ] || [ -e /dev/nvidia0 ]; then
    skp "the nvidia profile" \
        "this machine has the hardware, but the profile is distrobox's own --nvidia and there is nothing to hand podman directly"
else
    skp "the nvidia profile" "no NVIDIA device on this machine; --nvidia remains argv-pinned only"
fi

# ── the machine running the tests ───────────────────────────────────────────
section "no side effects"
# Filtered on this run's own name prefix, not a guess. An earlier version of
# this file filtered `^disp-` — the DISPOSABLE engine's prefix, which nothing
# here is ever called — so the assertion passed whether or not a container was
# leaked, and removing every `--rm` would not have reddened it.
#
# And the assertion refuses to speak when nothing ran: on a machine where the
# hardware sections all skipped, "no container was left behind" is true for the
# uninteresting reason, so it is reported as a skip rather than banked as a
# pass.
left="$(podman ps -a --filter "name=^$PROBE_NAME-" --format '{{.Names}}' 2>/dev/null)"
if [ "$started_containers" -eq 0 ]; then
    skp "no container was left behind by this suite" \
        "this suite started no container here, so there was nothing to leak"
elif [ -z "$left" ]; then
    ok "no container was left behind by this suite ($started_containers started, all --rm)"
else
    bad "no container was left behind by this suite" "still present: $(tr '\n' ' ' <<<"$left")"
fi

finish
