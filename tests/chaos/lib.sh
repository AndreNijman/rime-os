#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  The APEX chaos harness — verdicts, diagnostics, and the contract a case
#  must satisfy before the harness will believe anything it says.
#
#  Roadmap P1-062 criterion 3: "failures produce reproducible diagnostics and
#  do not silently corrupt the installation." That criterion is the one that
#  makes the other two worth having, so it is the one this file is about.
#
#  ═══ THE DEFECT THIS FILE EXISTS TO PREVENT ═══
#
#  A chaos harness that reports "no corruption" when it never managed to inject
#  the fault is worse than no harness. It is the same defect this repository
#  has already recorded under "permission denied is not absence" — a read that
#  could not be performed, reported as a fact — except with the stakes of a
#  reliability claim about somebody's operating system.
#
#  So: **every case carries a three-way verdict**, never a boolean.
#
#      survived         the fault was injected, PROVEN to be present by an
#                       observation independent of the injector, the subject
#                       was run, and the subject both kept its state intact
#                       and SAID something about the fault.
#      failed           the fault was injected and proven, the subject ran,
#                       and either its state is corrupt or it carried on
#                       silently. The reason names which.
#      could-not-inject the fault was not proven present, or the subject could
#                       not be run at all. Never a pass. Never a failure.
#
#  This mirrors `apexd/apex/src/verify.rs`'s `Verdict`, which is the house
#  pattern: `Verified` / `Failed` / `CouldNotRun`, and its rule that "any of
#  them failing to *run* is not a failure, and is not a pass either". The one
#  arm dropped here is `Absent`, which has no meaning for an injected fault.
#
#  ═══ THE TWO RULES THAT ARE NOT NEGOTIABLE ═══
#
#  1. `case_prove_injected` is an observation INDEPENDENT of the injector, and
#     is never the injector's exit code. `unshare --net` exiting 0 proves that
#     unshare ran, not that the network is gone; a `connect()` returning
#     ENETUNREACH inside the namespace proves the network is gone. The harness
#     cannot enforce independence syntactically, so every case states in its
#     header what its proof observes and why that is not the injector talking.
#
#  2. A `survived` requires the subject to have SAID something. The incident
#     this whole unit is built around is "a staged ostree deployment is
#     discarded by a crash before clean shutdown" — and the requirement
#     recorded in ROADMAP/state/agents/p1-062.md is that the machine "must come
#     back on the old image *and say so*". A subject that silently absorbs a
#     fault has not survived it; it has hidden it. `expect_says` is how a case
#     asserts that half, and `judge_silent` is the verdict when it is missing.
#
#  ═══ WHAT MAKES A RUN REPRODUCIBLE ═══
#
#  Every run writes a bundle per case under `$CHAOS_OUT/<case>/`:
#
#      verdict.json     {"case","state","reason",...} — the same shape as
#                       `Verdict::to_json`, so one jq expression reads both.
#      seed             the integer every random choice in the case derived from
#      env.txt          kernel, uid, userns/netns/tmpfs/kvm availability,
#                       `apex --version`, the git revision of the tree
#      inject.out/err   the injector's streams
#      observe.out/err  the subject's streams, which are the diagnostics a
#                       human reads when a case goes red
#      baseline.out     the subject's output with NO fault injected, so a red
#                       case can be told apart from a subject that was already
#                       saying that
#      state.before     sha256 of every file in the fixture root, sorted
#      state.after      the same, after the subject ran
#      state.diff       the lines that changed, which is the corruption claim
#
#  `run-chaos --replay <bundle>` re-runs from `seed` and the recorded revision
#  and diffs the verdicts. That is what "reproducible" is allowed to mean here.
#
#  ═══ WHAT THIS HARNESS MAY NOT DO ═══
#
#  AGENTS.md's boot-path rule applies to the harness itself, and
#  tests/test-apex-chaos.sh asserts it rather than trusting this comment: no
#  file under tests/chaos/ may name `bootctl`, `rpm-ostree`, `ostree admin`,
#  `efibootmgr`, or run `apex update`, outside a comment. Every case drives the
#  subject through a FIXTURE ROOT — `APEX_RECOVER_ROOT`, `APEX_TRUST_ROOT` and
#  friends — and the driver refuses a case that invokes `apex` without one.
# ─────────────────────────────────────────────────────────────────────────────

# Callers set -euo pipefail themselves; this file must be safe to source under
# it, and must not set it on their behalf.

CHAOS_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHAOS_REPO="$(cd "$CHAOS_LIB_DIR/../.." && pwd)"
export CHAOS_LIB_DIR CHAOS_REPO

# ── the three verdict states ────────────────────────────────────────────────
# Strings rather than an enum, because the bundle is JSON and a human reads it.
readonly CHAOS_SURVIVED="survived"
readonly CHAOS_FAILED="failed"
readonly CHAOS_CANNOT="could-not-inject"

chaos_log()  { printf '\n>>> %s\n' "$*" >&2; }
chaos_info() { printf '    %s\n' "$*" >&2; }
chaos_die()  { printf '!!! %s\n' "$*" >&2; exit 2; }

# ── prerequisites, answered once and recorded ───────────────────────────────
#
# Each is a REAL probe, not a `command -v`. `command -v unshare` succeeds on a
# runner whose kernel refuses an unprivileged user namespace — Ubuntu 24.04
# ships kernel.apparmor_restrict_unprivileged_userns=1 and pr-validation.yml
# already has to turn it off for bubblewrap. A prerequisite answered by
# probing for the tool rather than the capability is how a case ends up
# reporting "survived" having injected nothing.

chaos_have_userns() {
    unshare --user --map-root-user -- true >/dev/null 2>&1
}

chaos_have_netns() {
    # The capability, proven by looking: a namespace whose only interface is a
    # DOWN loopback. `unshare` exiting 0 is not the same claim.
    local out
    out="$(unshare --user --map-root-user --net -- ip -o link show 2>/dev/null || true)"
    [[ -n "$out" ]] && ! grep -qv '\blo\b' <<<"$out"
}

chaos_have_mountns_tmpfs() {
    # A size-limited tmpfs inside a user+mount namespace is the only way to
    # produce a genuine ENOSPC without root and without a loop device. Probed
    # by mounting one and asking df, because "mount exited 0" has been wrong
    # here before under a restricted seccomp profile.
    unshare --user --map-root-user --mount -- sh -c '
        d=$(mktemp -d) || exit 1
        mount -t tmpfs -o size=64k tmpfs "$d" 2>/dev/null || exit 1
        [ "$(stat -f -c %b "$d")" -gt 0 ] || exit 1
        exit 0' >/dev/null 2>&1
}

chaos_have_kvm() { [[ -r /dev/kvm && -w /dev/kvm ]]; }

chaos_have_apex_bin() { [[ -x "${APEX_BIN:-}" ]]; }

# chaos_prereq NAME — 0 if satisfied, 1 otherwise, and it prints the reason a
# case will carry into its could-not-inject verdict.
chaos_prereq() {
    case "$1" in
        apex-binary)
            chaos_have_apex_bin && return 0
            echo "the apex binary is not built (looked at ${APEX_BIN:-<unset>})"; return 1 ;;
        userns)
            chaos_have_userns && return 0
            echo "this kernel refuses an unprivileged user namespace"; return 1 ;;
        netns)
            chaos_have_netns && return 0
            echo "no unprivileged network namespace on this kernel"; return 1 ;;
        tmpfs-ns)
            chaos_have_mountns_tmpfs && return 0
            echo "cannot mount a size-limited tmpfs in a user+mount namespace"; return 1 ;;
        kvm)
            chaos_have_kvm && return 0
            echo "/dev/kvm is absent or not writable"; return 1 ;;
        *)
            echo "unknown prerequisite '$1' — the harness will not guess"; return 1 ;;
    esac
}

# ── the environment record every bundle carries ─────────────────────────────
chaos_write_env() {
    local out="$1"
    {
        printf 'harness-revision: %s\n' \
            "$(git -C "$CHAOS_REPO" rev-parse HEAD 2>/dev/null || echo unknown)"
        printf 'harness-dirty: %s\n' \
            "$(git -C "$CHAOS_REPO" status --porcelain 2>/dev/null | wc -l)"
        printf 'kernel: %s\n' "$(uname -sr)"
        printf 'uid: %s\n' "$(id -u)"
        printf 'os: %s\n' \
            "$(. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-unknown}" || echo unknown)"
        printf 'userns: %s\n'   "$(chaos_have_userns && echo yes || echo no)"
        printf 'netns: %s\n'    "$(chaos_have_netns && echo yes || echo no)"
        printf 'tmpfs-ns: %s\n' "$(chaos_have_mountns_tmpfs && echo yes || echo no)"
        printf 'kvm: %s\n'      "$(chaos_have_kvm && echo yes || echo no)"
        printf 'apex-bin: %s\n' "${APEX_BIN:-<unset>}"
        if chaos_have_apex_bin; then
            printf 'apex-version: %s\n' "$("$APEX_BIN" --version 2>&1 | head -1)"
        else
            printf 'apex-version: <binary absent>\n'
        fi
    } > "$out"
}

# ── state snapshots, which are the corruption claim ─────────────────────────
#
# A path the harness could not read is recorded as `<unreadable>` and NOT
# omitted. Omitting it would make a file that became unreadable look like a
# file that did not change, which is precisely the confusion this project
# names "permission denied is not absence".
chaos_snapshot() {
    local root="$1" out="$2"
    : > "$out"
    [[ -d "$root" ]] || { printf '<no such root> %s\n' "$root" > "$out"; return 0; }
    # `\( -type f -o -type l \)` with ONE -print0: the parenthesised form is
    # needed because `find -type f -o -type l -print0` binds -print0 to the
    # second arm only, and every regular file then vanishes from the snapshot —
    # which would read as "nothing changed" for a case that corrupted one.
    # `|| true` on both arms, and it is load-bearing rather than defensive: the
    # left side of a pipeline runs in a subshell that inherits the caller's
    # `set -e`, and `find | while read` exits 1 when `read` hits EOF — which is
    # the NORMAL end of the loop. Without it the subshell dies there, the
    # second find never runs, and the driver exits mid-case with no verdict and
    # no message. It did exactly that on this harness's first run.
    {
        find "$root" \( -type f -o -type l \) -print0 2>/dev/null \
        | while IFS= read -r -d '' f; do
            rel="${f#"$root"/}"
            if [[ -L "$f" ]]; then
                printf 'symlink:%s %s\n' "$(readlink "$f")" "$rel"
            elif [[ -r "$f" ]]; then
                printf '%s %s\n' "$(sha256sum < "$f" 2>/dev/null | cut -d' ' -f1)" "$rel"
            else
                printf '<unreadable> %s\n' "$rel"
            fi
        done || true
        # Directories the harness cannot descend into are a fact about the tree
        # and belong in the snapshot too; without them, a chmod 000 on a
        # directory reads as "every file under it vanished", which is the
        # inverse of the mistake this whole harness is about.
        find "$root" -type d ! -readable -printf 'dir:<unreadable> %P\n' 2>/dev/null || true
    } | LC_ALL=C sort > "$out"
}

# ── expectations a case records, which the driver turns into a verdict ──────
#
# A case never prints "ok" and never decides its own verdict. It records
# expectations; the driver reads the tally. That keeps the "could-not-inject
# beats everything" rule in one place instead of in every case.
CHAOS_EXPECT_OK=0
CHAOS_EXPECT_BAD=0
CHAOS_EXPECT_NOTES=()

chaos_reset_expectations() {
    CHAOS_EXPECT_OK=0
    CHAOS_EXPECT_BAD=0
    CHAOS_EXPECT_NOTES=()
}

_chaos_pass() {
    CHAOS_EXPECT_OK=$((CHAOS_EXPECT_OK + 1))
    CHAOS_EXPECT_NOTES+=("ok   $1")
    printf '      ok   %s\n' "$1" >&2
}

_chaos_fail() {
    CHAOS_EXPECT_BAD=$((CHAOS_EXPECT_BAD + 1))
    CHAOS_EXPECT_NOTES+=("BAD  $1")
    printf '      BAD  %s\n' "$1" >&2
}

# expect_says WHAT NEEDLE HAYSTACK — the subject must NAME the fault.
# This is the half of a `survived` that is about honesty rather than integrity.
expect_says() {
    local what="$1" needle="$2" hay="$3"
    if grep -qiF -- "$needle" <<<"$hay"; then _chaos_pass "$what"
    else _chaos_fail "$what (nothing matching '$needle' in the subject's output)"; fi
}

# expect_silent_about WHAT NEEDLE HAYSTACK — the subject must NOT make a claim.
expect_silent_about() {
    local what="$1" needle="$2" hay="$3"
    if grep -qiF -- "$needle" <<<"$hay"; then
        _chaos_fail "$what (the subject said '$needle', which it cannot know)"
    else _chaos_pass "$what"; fi
}

# expect_rc WHAT WANT GOT
expect_rc() {
    local what="$1" want="$2" got="$3"
    if [[ "$want" == "$got" ]]; then _chaos_pass "$what (rc=$got)"
    else _chaos_fail "$what: want rc=$want, got rc=$got"; fi
}

# expect_no_corruption WHAT BEFORE AFTER [ALLOW_REGEX]
#
# The installation must not be silently corrupted. Paths the case KNOWS the
# subject legitimately rewrites are named by ALLOW_REGEX; everything else
# changing is corruption. An empty allow list is the strict reading and is the
# default, because a case that has to widen it is a case that has learned
# something and should say so in its header.
expect_no_corruption() {
    local what="$1" before="$2" after="$3" allow="${4:-}"
    local diff_out
    diff_out="$(diff "$before" "$after" 2>/dev/null | grep -E '^[<>]' || true)"
    if [[ -n "$allow" ]]; then
        diff_out="$(grep -Ev "$allow" <<<"$diff_out" || true)"
    fi
    if [[ -z "$diff_out" ]]; then _chaos_pass "$what"
    else
        _chaos_fail "$what — these paths changed under the fault:"
        while IFS= read -r line; do
            [[ -n "$line" ]] && CHAOS_EXPECT_NOTES+=("       $line")
            [[ -n "$line" ]] && printf '           %s\n' "$line" >&2
        done <<<"$diff_out"
    fi
}

# expect_unchanged_from_baseline WHAT BASELINE OBSERVED
# The opposite assertion: the subject must have reacted at all. A subject whose
# output under the fault is byte-identical to its output without one has not
# noticed, whatever it printed.
expect_differs_from_baseline() {
    local what="$1" baseline="$2" observed="$3"
    if [[ "$baseline" == "$observed" ]]; then
        _chaos_fail "$what (byte-identical to the run with no fault injected)"
    else _chaos_pass "$what"; fi
}

# ── running the subject ─────────────────────────────────────────────────────
#
# Every invocation of the `apex` binary goes through here, and here is where
# the fixture-root rule is enforced: a case that forgot to redirect the subject
# at a fixture tree would run it against the developer's live machine. That is
# not a test failure, it is a machine-state accident, so it aborts the case.
CHAOS_FIXTURE_VARS=(
    APEX_TRUST_ROOT APEX_RECOVER_ROOT APEX_QUALIFY_ROOT APEX_DISPOSABLE_ROOT
    APEX_STORAGE_ROOT APEX_FIRMWARE_ROOT APEX_HOST_ROOT APEX_BOOT_ROOT
    APEX_SYS_ROOT APEX_DEVICES_ROOT
)

chaos_fixture_root_is_set() {
    local v
    for v in "${CHAOS_FIXTURE_VARS[@]}"; do
        [[ -n "${!v:-}" ]] && return 0
    done
    return 1
}

# chaos_run_subject OUTFILE ERRFILE -- CMD...
# Returns the command's rc; never aborts the driver.
chaos_run_subject() {
    local out="$1" err="$2"; shift 2
    [[ "${1:-}" == "--" ]] && shift
    chaos_fixture_root_is_set || chaos_die \
        "a case invoked the subject with no fixture root set; refusing to run it against this machine"
    local rc=0
    "$@" > "$out" 2> "$err" || rc=$?
    return "$rc"
}

# ── deterministic randomness ────────────────────────────────────────────────
#
# Everything a case chooses at random comes from here, so `--replay` with the
# recorded seed reproduces the same choices. `$RANDOM` seeded per case rather
# than per run: a case added in the middle of the list must not shift the
# choices every case after it makes.
chaos_seed_for_case() {
    local seed="$1" name="$2" h
    h="$(printf '%s/%s' "$seed" "$name" | sha256sum | cut -c1-8)"
    printf '%d\n' $((16#$h % 32768))
}

# chaos_rand LO HI — inclusive, from the case's seeded stream.
chaos_rand() {
    local lo="$1" hi="$2"
    echo $(( lo + (RANDOM % (hi - lo + 1)) ))
}

# ── the verdict record ──────────────────────────────────────────────────────
#
# Written with printf rather than jq so the bundle exists even on a machine
# with no jq, which is exactly the machine whose diagnostics somebody is about
# to read.
chaos_write_verdict() {
    local dir="$1" name="$2" state="$3" reason="$4" seed="$5" title="$6"
    local esc_reason esc_title
    esc_reason="$(printf '%s' "$reason" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr '\n' ' ')"
    esc_title="$(printf '%s' "$title" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr '\n' ' ')"
    {
        printf '{\n'
        printf '  "case": "%s",\n' "$name"
        printf '  "title": "%s",\n' "$esc_title"
        printf '  "state": "%s",\n' "$state"
        printf '  "reason": "%s",\n' "$esc_reason"
        printf '  "seed": %s,\n' "$seed"
        printf '  "expectationsPassed": %s,\n' "$CHAOS_EXPECT_OK"
        printf '  "expectationsFailed": %s,\n' "$CHAOS_EXPECT_BAD"
        printf '  "revision": "%s"\n' \
            "$(git -C "$CHAOS_REPO" rev-parse HEAD 2>/dev/null || echo unknown)"
        printf '}\n'
    } > "$dir/verdict.json"
}

# ── fixture machines ────────────────────────────────────────────────────────
#
# Whole machines presented as trees, the same shape tests/test-apex-channel.sh
# and tests/test-apex-recover.sh already use. They are here rather than copied
# into each case because a case that builds its own fixture is a case that can
# quietly build one the subject does not recognise, and then "the subject said
# nothing" means "the subject had nothing to say".
#
# Both take the root as $1 and leave it in the state a HEALTHY machine is in.
# The fault is what `case_inject` does to it afterwards.

CHAOS_CSUM=f3f505fc39fb268c59f4458365c96b764a7bd7d30f2f51e98bb6a009666b7852
CHAOS_BOOTCSUM=1d98b51dd76621b656c50e4f22dc7e5eade9b0f869443a3efa90eee08eb9373e
CHAOS_DIGEST=sha256:1111111111111111111111111111111111111111111111111111111111111111

# chaos_mk_trust_root ROOT [TAG] — what `apex trust` and `apex channel` read.
chaos_mk_trust_root() {
    local root="$1" tag="${2:-ghcr.io/andrenijman/apex-os:edge}"
    mkdir -p "$root/proc" "$root/etc/containers" \
             "$root/ostree/boot.0/default/$CHAOS_BOOTCSUM" \
             "$root/ostree/deploy/default/deploy/$CHAOS_CSUM.0" \
             "$root/var/lib/apex/channel"
    printf 'root=UUID=x rw ostree=/ostree/boot.0/default/%s/0\n' "$CHAOS_BOOTCSUM" \
        > "$root/proc/cmdline"
    ln -sfn "../../../deploy/default/deploy/$CHAOS_CSUM.0" \
        "$root/ostree/boot.0/default/$CHAOS_BOOTCSUM/0"
    printf '[origin]\ncontainer-image-reference=ostree-unverified-registry:%s\n' "$tag" \
        > "$root/ostree/deploy/default/deploy/$CHAOS_CSUM.0.origin"
    printf '{"default":[{"type":"insecureAcceptAnything"}]}\n' \
        > "$root/etc/containers/policy.json"
    printf '{"deployments":[{"booted":true,"base-commit-meta":{"ostree.manifest-digest":"%s"}}]}\n' \
        "$CHAOS_DIGEST" > "$root/rpm-ostree-status.json"
}

# chaos_write_update_record ROOT FROM_DIGEST — the note `apex update` leaves.
#
# Written the way `channel::record_update` writes it, because the fault this
# harness injects into it is a torn version of exactly these bytes.
chaos_write_update_record() {
    local root="$1" from="$2"
    mkdir -p "$root/var/lib/apex/channel"
    printf '{\n  "schema": 1,\n  "from_digest": "%s",\n  "tag": "edge",\n  "at": 1788700000\n}\n' \
        "$from" > "$root/var/lib/apex/channel/last-update.json"
}

# chaos_mk_recover_root ROOT — what `apex recover status` reads, healthy.
chaos_mk_recover_root() {
    local root="$1"
    local csum=8f14e45fceea167a5a36dedd4bea2543f14e45fceea167a5a36dedd4bea25431
    mkdir -p "$root/proc/net" "$root/run" "$root/etc" "$root/usr/share/apex-shell" \
             "$root/sys/firmware/efi/efivars" "$root/sys/bus/pci/devices/0000:03:00.0" \
             "$root/ostree/deploy/apex/deploy/${csum}.0" \
             "$root/ostree/deploy/apex/deploy/aaaa.0" \
             "$root/usr/lib/systemd/system" "$root/usr/libexec" "$root/var/lib/apex/pkg"
    printf 'BOOT_IMAGE=/vmlinuz root=UUID=x ostree=/ostree/boot.1/apex/%s/0 rw quiet\n' \
        "$csum" > "$root/proc/cmdline"
    printf 'NAME="APEX-OS"\nVERSION_ID=43\nVARIANT_ID=gaming\n' > "$root/etc/os-release"
    printf 'shell\n' > "$root/usr/share/apex-shell/shell.qml"
    : > "$root/usr/lib/systemd/system/rescue.target"
    printf '#!/bin/sh\nexit 0\n' > "$root/usr/libexec/apex-shell-firstrun"
    chmod +x "$root/usr/libexec/apex-shell-firstrun"
    printf '0x030000\n' > "$root/sys/bus/pci/devices/0000:03:00.0/class"
    printf '0x1002\n'   > "$root/sys/bus/pci/devices/0000:03:00.0/vendor"
    printf '0x1636\n'   > "$root/sys/bus/pci/devices/0000:03:00.0/device"
    printf '\x06\x00\x00\x00\x01' \
        > "$root/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c"
    : > "$root/run/ostree-booted"
    printf 'overlay / overlay ro,relatime 0 0\nnone /usr overlay ro,relatime 0 0\n' \
        > "$root/proc/mounts"
    printf 'amdgpu 1 0 - Live 0x0\ndrm 1 0 - Live 0x0\n' > "$root/proc/modules"
    printf 'Iface\tDestination\tGateway\nwlan0\t00000000\t0101A8C0\t0003\n' \
        > "$root/proc/net/route"
}

# ── structured expectations ─────────────────────────────────────────────────
#
# grep over a rendered report is how a case gets a false pass. It happened on
# this harness's first run, twice in one go:
#
#   * `expect_says "could not be read"` passed for the torn update record
#     because a DIFFERENT row — package extensions — carried that phrase;
#   * `expect_silent_about "nothing to roll back to"` failed for a subject that
#     was right, because the row explaining why it was NOT saying that quoted
#     the phrase inside the explanation.
#
# Both are the same mistake: asserting on prose that was written for a human.
# Where the subject offers a machine-readable document, the assertion reads
# THAT, and the prose assertion is kept only where it is the point (a message a
# person must see).
#
# expect_json WHAT FILE PYEXPR WANT — PYEXPR is evaluated with the parsed
# document bound to `d`; its repr is compared with WANT as a string. A document
# that will not parse is a failed expectation and says so, never a silent pass.
expect_json() {
    local what="$1" file="$2" expr="$3" want="$4" got
    if ! command -v python3 >/dev/null 2>&1; then
        _chaos_fail "$what (no python3, so the document could not be read)"
        return
    fi
    got="$(python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception as e:
    print("<unparseable: %s>" % e); sys.exit(0)
try:
    print(eval(sys.argv[2]))
except Exception as e:
    print("<no such field: %s>" % e)
' "$file" "$expr" 2>&1)"
    if [[ "$got" == "$want" ]]; then _chaos_pass "$what ($expr == $got)"
    else _chaos_fail "$what: $expr is '$got', wanted '$want'"; fi
}

# expect_json_nonempty WHAT FILE PYEXPR — the field must be there and carry
# something. Used for "the machine named the reason", where the reason's exact
# wording is not the assertion but its presence is.
expect_json_nonempty() {
    local what="$1" file="$2" expr="$3" got
    got="$(python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception as e:
    print("<unparseable: %s>" % e); sys.exit(0)
try:
    v = eval(sys.argv[2])
except Exception as e:
    print("<no such field: %s>" % e); sys.exit(0)
print("" if v is None else str(v))
' "$file" "$expr" 2>&1)"
    if [[ -n "$got" && "$got" != "<"* ]]; then
        _chaos_pass "$what ($expr = ${got:0:70})"
    else
        _chaos_fail "$what: $expr is '${got:-<empty>}'"
    fi
}
