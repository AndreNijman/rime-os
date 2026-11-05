#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  check-kernel-pin.sh — the kernel pin is only worth something if it is
#  actually pinned, and if the build actually reads it.
#
#  kernel/kernel.pin exists because the COPR build this replaces takes
#  `BuildRequires: dwarves` unversioned and fetches two of its three sources
#  from `master` branch URLs. That is how a kernel that could not load any
#  sched-ext scheduler shipped three times
#  (ROADMAP/evidence/kernel-build-20260920.md).
#
#  A pin file is exactly the kind of artefact that rots into decoration: a URL
#  quietly edited to a branch, a sha left behind when a version moves, a key
#  renamed in the Containerfile and left stale here. None of those fail a build
#  — the build just silently stops being reproducible, which is the original
#  defect wearing the fix's clothes.
#
#  So this runs without a build, and every check fails in BOTH directions:
#  a key the Containerfile reads but the pin does not define is an error, AND
#  a pin key nothing reads is an error.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

PIN=kernel/kernel.pin
CF=Containerfile.kernel

fail=0
err() { echo "FAIL: $*"; fail=$((fail + 1)); }
ok()  { echo "ok:   $*"; }

[ -f "$PIN" ] || { echo "FATAL: $PIN does not exist"; exit 1; }
[ -f "$CF" ]  || { echo "FATAL: $CF does not exist"; exit 1; }

# ── 1. every non-comment line is KEY=VALUE, and keys are unique ─────────────
declare -A VAL
while IFS= read -r line; do
    case "$line" in ''|\#*) continue ;; esac
    if [[ ! "$line" =~ ^([A-Z0-9_]+)=(.*)$ ]]; then
        err "$PIN: not KEY=VALUE: $line"
        continue
    fi
    k="${BASH_REMATCH[1]}"; v="${BASH_REMATCH[2]}"
    if [ -n "${VAL[$k]+x}" ]; then
        err "$PIN: $k defined twice — the second silently wins when sourced"
    fi
    VAL[$k]="$v"
done < "$PIN"
ok "$PIN parses, ${#VAL[@]} keys"

# ── 2. every *_URL has a *_SHA256 beside it, and every sha is a real sha ─────
# The pair is the whole point: a URL without a digest is a download, not a pin.
for k in "${!VAL[@]}"; do
    case "$k" in
        *_URL)
            base="${k%_URL}"
            # KERNEL_SRC_URL -> KERNEL_SRC_SHA256, DWARVES_RPM_URL -> DWARVES_RPM_SHA256
            sha="${base}_SHA256"
            if [ -z "${VAL[$sha]+x}" ]; then
                err "$k has no $sha — a URL without a digest is not a pin"
            else
                ok "$k is digest-pinned by $sha"
            fi
            ;;
    esac
done

for k in "${!VAL[@]}"; do
    case "$k" in
        *_SHA256)
            if [[ ! "${VAL[$k]}" =~ ^[0-9a-f]{64}$ ]]; then
                err "$k is not 64 lowercase hex characters: '${VAL[$k]}'"
            fi
            ;;
    esac
done

# ── 3. no URL may name a moving target ──────────────────────────────────────
# This is the specific defect inherited from upstream: its Source1 and Patch0
# are `…/master/…` URLs, so the content moves without the NVR changing. A
# permanent URL names a tag, a commit sha, or a koji NVR path.
for k in "${!VAL[@]}"; do
    case "$k" in
        *_URL)
            case "${VAL[$k]}" in
                */master/*|*/main/*|*/HEAD/*|*/latest/*)
                    err "$k points at a moving ref: ${VAL[$k]}"
                    ;;
                *)
                    ok "$k is a permanent URL"
                    ;;
            esac
            ;;
    esac
done

# ── 4. PAHOLE_VERSION_EXPECTED must follow DWARVES_NVR ──────────────────────
# These are two spellings of one fact, in two places, read by two different
# assertions — exactly the shape that drifts. 1.32-1.fc43 -> 132.
dw="${VAL[DWARVES_NVR]:-}"
pv="${VAL[PAHOLE_VERSION_EXPECTED]:-}"
if [ -z "$dw" ] || [ -z "$pv" ]; then
    err "DWARVES_NVR and PAHOLE_VERSION_EXPECTED must both be set"
else
    maj="${dw%%-*}"              # 1.32
    want="${maj%%.*}${maj#*.}"   # 1 + 32 -> 132
    if [ "$want" != "$pv" ]; then
        err "DWARVES_NVR=$dw implies CONFIG_PAHOLE_VERSION=$want, but PAHOLE_VERSION_EXPECTED=$pv"
    else
        ok "PAHOLE_VERSION_EXPECTED=$pv agrees with DWARVES_NVR=$dw"
    fi
fi

# The version that fixes the kfunc defect. Below this the build would produce
# the exact kernel this tier exists to stop shipping, and every other assertion
# here would still pass.
if [ -n "$pv" ] && [ "$pv" -lt 132 ] 2>/dev/null; then
    err "PAHOLE_VERSION_EXPECTED=$pv is below 132; 1.30 leaves 18 of 68 scx_bpf_* kfuncs untagged (see ROADMAP/evidence/kernel-build-20260920.md)"
fi

# ── 5. the pin and the Containerfile must agree, in both directions ─────────
# Keys the build reads. `${KERNEL_SRC_URL}` etc.
before=$fail
mapfile -t used < <(grep -oE '\$\{[A-Z0-9_]+\}' "$CF" | tr -d '${}' | sort -u)

# Names the Containerfile's own shell assigns, so they are locals and not pin
# keys. DERIVED from the file rather than listed by hand: a hand-maintained
# list makes every new local variable in the build a spurious failure here, and
# the pressure that creates is to keep widening the list until it stops
# catching the `${KERNEL_SRC_URI}` typo it exists for. (It was already 13 names
# long and two more locals broke it.)
declare -A LOCAL=()
while read -r a; do
    [ -n "$a" ] && LOCAL[$a]=1
done < <(grep -oE '(^|[[:space:];&|(])[A-Z][A-Z0-9_]*=' "$CF" \
         | grep -oE '[A-Z][A-Z0-9_]*' | sort -u)

# These come from outside the shell entirely: a Docker ARG, or /etc/os-release.
NOT_FROM_PIN="RIME_KERNEL_IMAGE|VERSION_ID"

for u in "${used[@]}"; do
    [[ "$u" =~ ^($NOT_FROM_PIN)$ ]] && continue
    # A name that is both a pin key and assigned by the build's own shell is
    # shadowing: the pin would be silently overridden by whatever the RUN set,
    # and every other assertion here would still pass.
    if [ -n "${LOCAL[$u]+x}" ] && [ -n "${VAL[$u]+x}" ]; then
        err "$CF assigns $u itself, but $PIN also defines it — the local shadows the pin"
        continue
    fi
    [ -n "${LOCAL[$u]+x}" ] && continue
    if [ -z "${VAL[$u]+x}" ]; then
        err "$CF reads \${$u} but $PIN does not define it — it would expand empty"
    fi
done
[ "$fail" -eq "$before" ] && ok "$CF reads no undefined pin key"

before2=$fail
for k in "${!VAL[@]}"; do
    # BUILDER_BASE is documentation of which base the Containerfile hardcodes;
    # it is checked separately below rather than expanded.
    [ "$k" = BUILDER_BASE ] && continue
    if ! grep -qF "\${$k}" "$CF"; then
        err "$PIN defines $k but nothing in $CF reads it — a pin nothing reads is decoration"
    fi
done
[ "$fail" -eq "$before2" ] && ok "every pin key is read by $CF"

# BUILDER_BASE is the one value the Containerfile must spell literally (a FROM
# cannot read a file), so assert the two say the same thing rather than letting
# them drift apart silently.
bb="${VAL[BUILDER_BASE]:-}"
if [ -n "$bb" ]; then
    if grep -qE "^FROM +${bb//./\\.}( |\$)" "$CF"; then
        ok "BUILDER_BASE=$bb matches $CF's FROM lines"
    else
        err "BUILDER_BASE=$bb but no matching FROM in $CF"
    fi
fi

# The spec names the tree it builds on its own (Version, %{_tag}), and
# rpmbuild fetches Source0 by that name when the pinned tarball's name differs.
# A pin bump that leaves the spec behind therefore compiles the OLD kernel with
# every sha256 above still passing (kernel-build 41966562281: pin 7.2.11-1,
# RPMs 7.2.9). The pin and the spec must name the same tree.
SPEC=kernel/kernel-cachyos.spec
if [ -f "$SPEC" ]; then
    sbase="$(sed -n 's/^%define _basekver[[:space:]]\+//p' "$SPEC")"
    sstable="$(sed -n 's/^%define _stablekver[[:space:]]\+//p' "$SPEC")"
    stagrel="$(sed -n 's/^%define _tag cachyos-%{_tarkver}-\([0-9]\+\)$/\1/p' "$SPEC")"
    sver="$sbase.$sstable"
    if [ "$sver" = "${VAL[KERNEL_VERSION]:-}" ]; then
        ok "$SPEC builds $sver, the pin's KERNEL_VERSION"
    else
        err "$SPEC builds '$sver' but $PIN says KERNEL_VERSION=${VAL[KERNEL_VERSION]:-}"
    fi
    if [ -n "$stagrel" ] && [ "cachyos-$sver-$stagrel" = "${VAL[KERNEL_TAG]:-}" ]; then
        ok "$SPEC's Source0 tag is cachyos-$sver-$stagrel, the pin's KERNEL_TAG"
    else
        err "$SPEC's Source0 tag is 'cachyos-$sver-$stagrel' but $PIN says KERNEL_TAG=${VAL[KERNEL_TAG]:-}"
    fi
else
    err "$SPEC does not exist"
fi

echo
if [ "$fail" -gt 0 ]; then
    echo "kernel pin: $fail problem(s)"
    exit 1
fi
echo "kernel pin: all checks passed"
