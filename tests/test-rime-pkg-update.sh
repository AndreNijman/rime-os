#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-pkg-update.sh — does a fix baked into a Rime image ever reach a
#  machine that is already running one?
#
#  ── The defect this exists for ──────────────────────────────────────────────
#  It did not. Measured on katana on 2026-09-19
#  (ROADMAP/evidence/katana-image-qual-20260919.md §0.2 and §1.2): the machine
#  was rebased onto the image carrying the pkg-share fix, rebooted, and
#  rime-sysext-rebuild.service — the unit whose entire job is to notice the OS
#  moved — started and finished in the SAME SECOND, reported success, and
#  re-merged a byte-identical old extension. 177 image paths still shadowed, 14
#  of them a 32-bit binary over a 64-bit one, and `ls /usr/share/vulkan/icd.d/ |
#  grep -c i686` still 0 on a machine that had just taken the update.
#
#  Two guards decide whether anything gets rebuilt, and before this suite
#  existed they checked different things:
#
#    cmd_rebuild --if-needed   os_version_id  yes | resolved set  NO | level yes
#    ext_up_to_date            os_version_id  yes | resolved set yes | level  NO
#
#  Neither can see a Rime image build on its own. `os_version` is VERSION_ID
#  out of /usr/lib/os-release, which is `43` before and after; the resolved set
#  comes from Fedora's repositories, which have nothing to do with what Rime
#  baked. PKG_COMPAT_LEVEL is the only signal that says "the image changed", and
#  the guard on the path every install, remove, upgrade and rebuild funnels
#  through did not read it.
#
#  ── The trap, which this suite is shaped to catch ───────────────────────────
#  Bumping PKG_COMPAT_LEVEL on its own APPEARS to fix this and does not.
#  --if-needed would see the mismatch, log "extension compatibility changed …
#  rebuilding", call rebuild_extension, re-download every rpm, stop at "already
#  up to date" because the resolved set is unchanged — and then write_state
#  would stamp the NEW level into state.json. The next boot sees a match and
#  no-ops for ever. One wasted download, no rebuild, and the marker silently
#  consumed: a fix that hides the fact that it did nothing.
#
#  So `the extension rebuilt after a bump` is not an assertion worth making. A
#  suite that only checks that passes against the BROKEN engine on any machine
#  whose repositories have drifted — which is exactly how katana's rebuild
#  eventually happened, by luck rather than design (evidence §2.1).
#
#  ── The discriminator ───────────────────────────────────────────────────────
#  Every case below HOLDS THE RESOLVED RPM SET CONSTANT and varies only
#  pkg_compat_level. A rebuild that happens under those conditions can only have
#  been caused by the level. Leg B prints the stored set and the computed set so
#  "constant" is a measurement rather than a claim.
#
#  ── And two mutants, so neither leg can pass for free ───────────────────────
#  Each leg re-runs its key case against ext_up_to_date with the level clause
#  DELETED from the live function body, and requires the defect back: the host
#  leg requires "up to date" where it just said "rebuild", and the container leg
#  requires the full §0.2 trap — "compatibility changed" followed by "already up
#  to date", nothing extracted, and the new level stamped into state.json
#  anyway. That is the ordering proof the evidence asks for: it demonstrates on
#  every run that the bump alone does nothing.
#
#  ── Two legs, and what each costs ───────────────────────────────────────────
#  LEG A runs anywhere: it sources the shipped engine and calls the real
#  ext_up_to_date against state files in a temp directory. No root, no network,
#  no container, nothing merged.
#
#  LEG B runs the real cmd_rebuild --if-needed — the service's exact ExecStart —
#  inside `podman run --rm`, because PKG_ROOT is a readonly constant pointing at
#  /var/lib/rime/pkg and the entrypoint needs root. Only the download and the
#  build are stubbed; state.json, the requested list, the merge marker, the
#  extension payload, os-release and write_state are all real, because "the
#  marker was consumed without a rebuild" is a statement about what write_state
#  put on disk.
#
#  PASS = every case behaves as listed AND both mutants reproduce the defect.
#
#  Run from anywhere: ./tests/test-rime-pkg-update.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# `set +e` for the same reason as every sibling suite: this one COUNTS failures
# instead of aborting, and GitHub Actions invokes a script as `bash -e {0}`,
# under which the first deliberately-failing command would truncate the run.
set +e
cd "$(dirname "$0")/.." || exit 2

ENGINE=files/system/libexec/rime-pkg
[ -f "$ENGINE" ] || { echo "cannot find $ENGINE"; exit 2; }
command -v jq >/dev/null || { echo "FATAL: jq is absent; every case here reads a state.json"; exit 2; }

WORK=$(mktemp -d /tmp/rime-pkg-update-test.XXXXXX) || exit 2
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0; skip=0
ok()      { printf 'PASS  %-58s\n' "$1"; pass=$((pass+1)); }
bad()     { printf 'FAIL  %-58s %s\n' "$1" "$2"; fail=$((fail+1)); }
skipped() { printf 'SKIP  %-58s %s\n' "$1" "$2"; skip=$((skip+1)); }

# ── the engine's own constant, never a literal ───────────────────────────────
# Reading it means the cases below keep meaning what they say after the next
# bump. A suite that hardcoded 3 would start asserting history.
LEVEL="$(bash -c 'e=$1; set --; source "$e" >/dev/null 2>&1; printf "%s" "${PKG_COMPAT_LEVEL:-}"' _ "$ENGINE")"
case "$LEVEL" in
    ''|*[!0-9]*) echo "FATAL: the engine's PKG_COMPAT_LEVEL did not read back as a number (got '${LEVEL}')"; exit 2 ;;
esac
BEHIND=$((LEVEL - 1))
AHEAD=$((LEVEL + 1))
printf 'engine PKG_COMPAT_LEVEL = %s\n\n' "$LEVEL"

# ═════════════════════════════════════════════════════════════════════════════
#  LEG A — the decision itself, against state files that differ in ONE field
# ═════════════════════════════════════════════════════════════════════════════

# The one resolved set every case in this leg uses. Written once, read twice:
# into every state.json and into the new_set argument, so no case can differ by
# a package even if someone edits one of them.
SET=(alpha-1.0-1.fc43.x86_64 beta-2.0-1.fc43.x86_64 gamma-3.0-1.fc43.i686)
NEWSET="$(printf '%s\n' "${SET[@]}" | LC_ALL=C sort)"
OTHERSET="$(printf '%s\n' "${SET[@]}" beta-2.0-2.fc43.x86_64 | LC_ALL=C sort)"

# The image's x86_64 twin of SET's one i686 package, as the engine records it
# (`image_twins`). REC_TWINS is what a state.json holds; NOW_TWINS is what the
# running image answers. Held equal everywhere except the cases about them.
TWIN_OLD="gamma-3.0-1.fc43"
TWIN_NEW="gamma-3.1-1.fc43"
REC_TWINS="$TWIN_OLD"

# $1 = output path, $2 = level ("none" omits the key), $3 = os_version_id,
# rest = the resolved NEVRAs. $REC_TWINS = recorded twins ("none" omits the key).
mkstate() {
    local out=$1 lvl=$2 osv=$3; shift 3
    local resolved twins
    resolved="$(printf '%s\n' "$@" | jq -R -s 'split("\n")|map(select(length>0))')"
    twins="$(printf '%s\n' "$REC_TWINS" | jq -R -s 'split("\n")|map(select(length>0))')"
    if [ "$lvl" = none ]; then
        jq -n --argjson r "$resolved" --arg v "$osv" \
           '{requested:["alpha","beta"],resolved:$r,os_id:"fedora",os_version_id:$v}' > "$out"
    else
        jq -n --argjson r "$resolved" --arg v "$osv" --argjson l "$lvl" \
           '{requested:["alpha","beta"],resolved:$r,os_id:"fedora",os_version_id:$v,pkg_compat_level:$l}' > "$out"
    fi
    if [ "$REC_TWINS" != none ]; then
        jq --argjson t "$twins" '. + {image_twins:$t}' "$out" > "$out.t" && mv -f "$out.t" "$out"
    fi
}

# Calls the REAL ext_up_to_date out of the shipped engine. Each case gets its
# own `bash -c`: the engine's `set -euo pipefail` and its readonly constants
# both leak into whatever sources it, so re-sourcing in one shell would abort on
# the second readonly. `set --` first, because sourcing reaches `main`.
#   $1 state file, $2 new_set, $3 merged rc, $4 current_payload, $5 os_version,
#   $6 optional: `mutant` to delete the level clause from the function body.
judge() {
    local state=$1 newset=$2 mrc=$3 payload=$4 osv=$5 mode=${6:-real}
    MERGED_RC="$mrc" PAYLOAD="$payload" OSVER="$osv" MODE="$mode" TWINS="${NOW_TWINS-$TWIN_OLD}" \
    bash -c '
        e=$1; st=$2; ns=$3; set --
        source "$e" >/dev/null 2>&1
        set +e
        merged() { return "$MERGED_RC"; }
        current_payload() { printf "%s" "$PAYLOAD"; }
        os_version() { printf "%s" "$OSVER"; }
        image_twins() { printf "%s\n" "$TWINS"; }
        if [ "$MODE" = twins-mutant ]; then
            body="$(declare -f ext_up_to_date | grep -v twins_unchanged)"
            eval "$body"
        fi
        if [ "$MODE" = mutant ]; then
            # The pre-fix function, reconstructed from the shipped one rather
            # than copied: delete the only line that mentions the level.
            body="$(declare -f ext_up_to_date | grep -v pkg_compat_level)"
            eval "$body"
        fi
        ext_up_to_date "$st" "$ns"
        exit $?
    ' _ "$ENGINE" "$state" "$newset"
}

# $1 = case name, $2 = expected rc (0 = "up to date", 1 = "rebuild"), rest -> judge
expect() {
    local name=$1 want=$2; shift 2
    local rc; judge "$@"; rc=$?
    if [ "$rc" = "$want" ]; then ok "$name"
    else bad "$name" "ext_up_to_date returned $rc, expected $want"; fi
}

MERGED_PAYLOAD=/var/lib/extensions/rime-user.raw

echo "── leg A: one state field at a time, resolved set held constant ──"

# THE CONTROL FOR THE WHOLE LEG. Without it every "must rebuild" below could be
# passing because the stubs broke and the function returns 1 unconditionally.
mkstate "$WORK/a-match.json"   "$LEVEL"  43 "${SET[@]}"
expect "control: level ${LEVEL}, same set, merged — is up to date" 0 \
       "$WORK/a-match.json" "$NEWSET" 0 "$MERGED_PAYLOAD" 43

# THE DELIVERABLE. Identical resolved set, identical os_version_id, merged —
# only the level differs, exactly as it does on a machine that has just taken an
# image update.
mkstate "$WORK/a-behind.json"  "$BEHIND" 43 "${SET[@]}"
expect "level ${BEHIND} vs ${LEVEL}, same set — must rebuild" 1 \
       "$WORK/a-behind.json" "$NEWSET" 0 "$MERGED_PAYLOAD" 43

# A rollback onto an older image. An extension built by a NEWER engine is not
# "current" either, and `=` rather than `-lt` is what makes that true.
mkstate "$WORK/a-ahead.json"   "$AHEAD"  43 "${SET[@]}"
expect "level ${AHEAD} vs ${LEVEL}, same set — must rebuild" 1 \
       "$WORK/a-ahead.json" "$NEWSET" 0 "$MERGED_PAYLOAD" 43

# A state.json written before the field existed must read as older than
# anything, not as a match. `// 0` in the engine is what does this.
mkstate "$WORK/a-none.json"    none      43 "${SET[@]}"
expect "no pkg_compat_level at all — must rebuild" 1 \
       "$WORK/a-none.json" "$NEWSET" 0 "$MERGED_PAYLOAD" 43

# The other three legs of the decision, so the refactor that moved it into a
# function cannot have quietly dropped one.
expect "control: level ${LEVEL}, DIFFERENT set — must rebuild" 1 \
       "$WORK/a-match.json" "$OTHERSET" 0 "$MERGED_PAYLOAD" 43
mkstate "$WORK/a-osver.json"   "$LEVEL"  42 "${SET[@]}"
expect "control: level ${LEVEL}, different VERSION_ID — must rebuild" 1 \
       "$WORK/a-osver.json" "$NEWSET" 0 "$MERGED_PAYLOAD" 43
expect "control: not merged — must rebuild" 1 \
       "$WORK/a-match.json" "$NEWSET" 1 "$MERGED_PAYLOAD" 43
expect "control: no extension payload on disk — must rebuild" 1 \
       "$WORK/a-match.json" "$NEWSET" 0 "" 43
expect "control: no state.json — must rebuild" 1 \
       "$WORK/no-such-state.json" "$NEWSET" 0 "$MERGED_PAYLOAD" 43

# katana's measured state, as a literal. Level 2 is what
# /var/lib/rime/pkg/state.json held on 2026-09-19 on a machine with 177 image
# paths shadowed and zero i686 Vulkan ICDs merged over /usr. This is the only
# hardcoded number in the file and it is deliberate: it is a fact about a
# machine, not about this build. An engine that carries the pkg-share fix and
# is still at level 2 leaves every such machine exactly where katana was, so
# going back to 2 must fail here rather than pass quietly.
if [ "$LEVEL" -gt 2 ]; then
    mkstate "$WORK/a-katana.json" 2 43 "${SET[@]}"
    expect "katana's level-2 state against this engine — must rebuild" 1 \
           "$WORK/a-katana.json" "$NEWSET" 0 "$MERGED_PAYLOAD" 43
else
    bad "katana's level-2 state against this engine — must rebuild" \
        "PKG_COMPAT_LEVEL is ${LEVEL}: an engine carrying the pkg-share fix must be past 2, or every machine that measured 2 keeps its old extension"
fi

# ── the mutant: prove the clause is what produced the answers above ──────────
judge "$WORK/a-behind.json" "$NEWSET" 0 "$MERGED_PAYLOAD" 43 mutant
mrc=$?
if [ "$mrc" = 0 ]; then
    ok "mutant: with the level clause deleted, level ${BEHIND} reads as up to date"
else
    bad "mutant: with the level clause deleted, level ${BEHIND} still returned $mrc" \
        "so the assertions above are not measuring that clause and this suite proves nothing"
fi

# ── the image's own 32-bit twins moved (a driver bump) ───────────────────────
# katana, 2026-10-11: image xorg-x11-drv-nvidia-libs 615.71.09 -> 615.78.08, the
# extension's i686 copy stayed at 615.71.09, Steam died with "NVRM: API
# mismatch". OS version, level, merge state and the resolved set were ALL
# unchanged. Only the image's x86_64 twin differs.
NOW_TWINS="$TWIN_NEW" expect "image twin moved, everything else equal — must rebuild" 1 \
       "$WORK/a-match.json" "$NEWSET" 0 "$MERGED_PAYLOAD" 43
NOW_TWINS="$TWIN_OLD" expect "control: image twin unchanged — is up to date" 0 \
       "$WORK/a-match.json" "$NEWSET" 0 "$MERGED_PAYLOAD" 43
REC_TWINS=none
mkstate "$WORK/a-notwins.json" "$LEVEL" 43 "${SET[@]}"
REC_TWINS="$TWIN_OLD"
expect "state.json without image_twins (pre-fix machine) — must rebuild" 1 \
       "$WORK/a-notwins.json" "$NEWSET" 0 "$MERGED_PAYLOAD" 43
# A set with no 32-bit package has no twins to track: nothing recorded, nothing
# reported, and that must read as a match rather than a permanent rebuild.
REC_TWINS=""
mkstate "$WORK/a-native.json" "$LEVEL" 43 alpha-1.0-1.fc43.x86_64
NOW_TWINS="" expect "no i686 package at all: empty twins on both sides — is up to date" 0 \
       "$WORK/a-native.json" "$(printf 'alpha-1.0-1.fc43.x86_64')" 0 "$MERGED_PAYLOAD" 43
REC_TWINS="$TWIN_OLD"

NOW_TWINS="$TWIN_NEW" judge "$WORK/a-match.json" "$NEWSET" 0 "$MERGED_PAYLOAD" 43 twins-mutant
mrc=$?
if [ "$mrc" = 0 ]; then
    ok "mutant: with the twins clause deleted, a moved driver reads as up to date"
else
    bad "mutant: with the twins clause deleted, a moved driver still returned $mrc" \
        "so the twin cases above are not measuring that clause"
fi

# ═════════════════════════════════════════════════════════════════════════════
#  LEG B — rime-sysext-rebuild.service's exact ExecStart, end to end
# ═════════════════════════════════════════════════════════════════════════════
echo
echo "── leg B: cmd_rebuild --if-needed, in a container, set held constant ──"

PROBE="$WORK/probe.sh"
cat > "$PROBE" <<'PROBE_EOF'
set -uo pipefail
ENGINE=/repo/files/system/libexec/rime-pkg

# Everything the decision reads is REAL in here: a real merge marker, a real
# /usr/lib/os-release, a real state.json, a real requested list, a real
# extension payload, and the engine's own write_state. Only the download and the
# build are replaced, because a real one needs a network and minutes and this
# suite is about the decision in front of it.
mkdir -p /usr/lib/extension-release.d /var/lib/extensions /var/lib/rime/pkg
: > /usr/lib/extension-release.d/extension-release.rime-user
echo 'the extension that was already merged' > /var/lib/extensions/rime-user.raw

OSVER="$( . /usr/lib/os-release; printf '%s' "$VERSION_ID" )"
SET='alpha-1.0-1.fc43.x86_64
beta-2.0-1.fc43.x86_64
gamma-3.0-1.fc43.i686'
OTHER="$SET
delta-4.0-1.fc43.x86_64"

# shellcheck disable=SC1090
set --
source "$ENGINE" >/dev/null 2>&1
set +e

LEVEL="$PKG_COMPAT_LEVEL"
BEHIND=$((LEVEL - 1))
echo "PROBE_LEVEL $LEVEL"

# The live function, kept so the mutant can be undone exactly.
REAL_UP_TO_DATE="$(declare -f ext_up_to_date)"

# ── the stubs, and only these ───────────────────────────────────────────────
# `rpm -q NAME` answers the "is this now in the image?" probe: no, so nothing is
# dropped from the requested list. `rpm -qp` reads the NEVRA back out of the
# file name download_rpms wrote, which is how the computed set is made equal to
# the stored one by construction.
# `-qa` answers what the image installs: the x86_64 twin of gamma, whose build the
# probe moves to model a driver bump. Nothing else is installed.
IMAGE_GAMMA=3.0-1.fc43
rpm() {
    case "${1:-}" in
        -qa) printf 'gamma x86_64 %s\n' "$IMAGE_GAMMA" ;;
        -q)  return 1 ;;
        -qp) local f="${!#}" b; b="${f##*/}"; printf '%s\n' "${b%.rpm}" ;;
        *)   return 1 ;;
    esac
}
CURRENT_SET="$SET"
download_rpms() { local dest=$1 n; for n in $CURRENT_SET; do : > "${dest}/${n}.rpm"; done; }
stage_local_rpms() { :; }
guard_rpms() { :; }
extract_rpms() { : > /tmp/extracted; }
fix_caches() { :; }
split_out_of_image() { :; }
write_extension_release() { :; }
label_tree() { :; }
build_payload() { local out="${2}/new.raw"; printf 'rebuilt %s\n' "$(date +%s%N)" > "$out"; printf '%s' "$out"; }
install_etc() { :; }
install_var_dirs() { :; }
gc_local_cache() { :; }
# The real one runs `systemd-sysext refresh`, which cannot work in a container
# and would `die`. The move it performs is kept, because the payload's sha256 is
# how a rebuild is detected below.
activate_payload() { mv -f "$1" "$EXT_IMAGE"; }

REC_TWIN=gamma-3.0-1.fc43   # what a state.json recorded the image to hold
mkstate() {   # $1 = level, $2 = the resolved set to store
    local lvl=$1 set_text=$2 resolved
    resolved="$(printf '%s\n' $set_text | jq -R -s 'split("\n")|map(select(length>0))')"
    jq -n --argjson r "$resolved" --arg v "$OSVER" --argjson l "$lvl" --arg t "$REC_TWIN" \
       '{requested:["alpha","beta","gamma"],resolved:$r,local_files:[],unsigned_accepted:[],
         os_id:"fedora",os_version_id:$v,built:"2026-09-19T01:43:06Z",
         image:"/var/lib/extensions/rime-user.raw",image_sha256:"stale",image_bytes:0,
         pkg_compat_level:$l,image_twins:[$t]}' > /var/lib/rime/pkg/state.json
    printf 'alpha\nbeta\ngamma\n' > /var/lib/rime/pkg/requested
}

run_case() {  # $1 label, $2 level to store, $3 argv, $4 optional set to STORE
    # The stored set and the downloaded set are separate arguments on purpose:
    # storing whatever was about to be downloaded would make every case a match
    # by construction, and the one case that varies the package set would then
    # silently assert the opposite of what it says.
    local label=$1 lvl=$2 argv=$3 stored_set=${4:-$SET}
    local before after out rc extracted=no uptodate=no changed=no announced=no
    rm -f /tmp/extracted
    mkstate "$lvl" "$stored_set"
    before="$(sha256sum "$EXT_IMAGE" | cut -d' ' -f1)"
    if [ "$argv" = if-needed ]; then
        out="$(cmd_rebuild --if-needed 2>&1)"; rc=$?
    else
        out="$(cmd_rebuild 2>&1)"; rc=$?
    fi
    after="$(sha256sum "$EXT_IMAGE" | cut -d' ' -f1)"
    [ -e /tmp/extracted ] && extracted=yes
    # NOT `printf … | grep -q`: under pipefail a match kills the writer with
    # SIGPIPE and the `if` is handed a 141, position-dependent and silent.
    [[ "$out" == *"already up to date"* ]] && uptodate=yes
    [[ "$out" == *"extension compatibility changed"* ]] && announced=yes
    [ "$before" != "$after" ] && changed=yes
    echo "PROBE_CASE ${label} rc=${rc} extracted=${extracted} uptodate=${uptodate} announced=${announced} payload_changed=${changed} state_level=$(jq -r '.pkg_compat_level // 0' /var/lib/rime/pkg/state.json) state_twin=$(jq -r '.image_twins[0] // "none"' /var/lib/rime/pkg/state.json)"
}

# ── the set really is constant: stored vs computed, both printed ────────────
mkstate "$LEVEL" "$CURRENT_SET"
stored="$(jq -r '.resolved[]' /var/lib/rime/pkg/state.json | LC_ALL=C sort)"
probe_dir=/tmp/setprobe; rm -rf "$probe_dir"; mkdir -p "$probe_dir"
download_rpms "$probe_dir"
computed="$(for f in "$probe_dir"/*.rpm; do rpm -qp --nosignature --qf '%{NAME}-%{EVR}.%{ARCH}\n' "$f"; done | LC_ALL=C sort)"
if [ "$stored" = "$computed" ]; then echo "PROBE_SETMATCH yes"; else echo "PROBE_SETMATCH no"; fi
echo "PROBE_SETSIZE $(printf '%s\n' "$computed" | wc -l)"

# 1. THE DELIVERABLE: the service's exact ExecStart, on a machine whose
#    extension was built one level ago and whose repositories have NOT drifted.
run_case image-update "$BEHIND" if-needed

# 2. A normal boot with nothing to do must still cost nothing.
run_case normal-boot "$LEVEL" if-needed

# 3. Bare `rebuild` at a matching level: this is the one that reaches
#    ext_up_to_date's short-circuit and must still stop there, so case 1 cannot
#    be passing because the short-circuit is simply dead.
run_case bare-match "$LEVEL" bare

# 4. A changed package set at a matching level still rebuilds — the leg of the
#    decision that already worked, asserted so the refactor cannot have eaten
#    it. The state stores the OLD set; the download produces the new one.
CURRENT_SET="$OTHER"
run_case bare-newset "$LEVEL" bare "$SET"
CURRENT_SET="$SET"

# 5. THE MUTANT: the pre-fix engine, reconstructed by deleting the one line that
#    mentions the level, re-running case 1. This is §0.2's trap end to end.
eval "$(printf '%s\n' "$REAL_UP_TO_DATE" | grep -v pkg_compat_level)"
run_case mutant "$BEHIND" if-needed
eval "$REAL_UP_TO_DATE"

# 6. THE DRIVER BUMP (katana, 2026-10-11): level, OS version, merge state and the
#    resolved set all match; only the image's x86_64 twin moved. The extension
#    must be rebuilt and the NEW twin recorded.
IMAGE_GAMMA=3.1-1.fc43
run_case driver-bump "$LEVEL" if-needed
# 7. ...and having recorded it, the next boot costs nothing. run_case writes the
#    state itself, so this one is stored as the bumped machine would hold it.
IMAGE_GAMMA=3.1-1.fc43; REC_TWIN=gamma-3.1-1.fc43
run_case twin-recorded "$LEVEL" if-needed

# 8. THE MUTANT: no twins clause anywhere. The driver bump must then rebuild
#    nothing and announce nothing: the defect, reproduced.
IMAGE_GAMMA=3.1-1.fc43; REC_TWIN=gamma-3.0-1.fc43
REAL_TWINS="$(declare -f twins_unchanged)"
twins_unchanged() { return 0; }
run_case twins-mutant "$LEVEL" if-needed
eval "$REAL_TWINS"
PROBE_EOF

if ! command -v podman >/dev/null 2>&1; then
    skipped "leg B: cmd_rebuild --if-needed end to end" "podman is absent"
else
    IMAGE=${RIME_PKG_UPDATE_IMAGE:-registry.fedoraproject.org/fedora:43}
    out=$(podman run --rm \
            -v "$PWD":/repo:ro,Z -v "$PROBE":/probe.sh:ro,Z \
            "$IMAGE" bash -c 'command -v jq >/dev/null || dnf -y install jq >/dev/null 2>&1; bash /probe.sh' 2>&1)
    prc=$?

    field() { printf '%s\n' "$out" | sed -n "s/^PROBE_CASE $1 //p" | tr ' ' '\n' | sed -n "s/^$2=//p"; }

    if [ "$prc" != 0 ] && [[ "$out" != *PROBE_CASE* ]]; then
        bad "leg B: the container probe ran" "podman exited ${prc}: $(printf '%s\n' "$out" | tail -3 | tr '\n' ' ')"
    else
        printf '      container PKG_COMPAT_LEVEL: %s\n' "$(printf '%s\n' "$out" | sed -n 's/^PROBE_LEVEL //p')"
        printf '%s\n' "$out" | sed -n 's/^PROBE_CASE /      /p'

        # The instrument first: if the stored and computed sets differ, every
        # rebuild below could be a package update and this leg measures nothing.
        setmatch="$(printf '%s\n' "$out" | sed -n 's/^PROBE_SETMATCH //p')"
        setsize="$(printf '%s\n' "$out" | sed -n 's/^PROBE_SETSIZE //p')"
        if [ "$setmatch" = yes ] && [ "${setsize:-0}" -gt 0 ]; then
            ok "the resolved set is held constant (${setsize} packages, stored = computed)"
        else
            bad "the resolved set is NOT held constant (match=${setmatch:-?}, size=${setsize:-?})" \
                "every rebuild below could be a package change instead of the compat level"
        fi

        # 1. The deliverable.
        if [ "$(field image-update extracted)" = yes ] && [ "$(field image-update payload_changed)" = yes ] \
           && [ "$(field image-update announced)" = yes ] && [ "$(field image-update uptodate)" = no ] \
           && [ "$(field image-update state_level)" = "$LEVEL" ]; then
            ok "image update: --if-needed rebuilt and stamped level ${LEVEL}"
        else
            bad "image update: --if-needed did not rebuild" \
                "extracted=$(field image-update extracted) payload_changed=$(field image-update payload_changed) uptodate=$(field image-update uptodate) state_level=$(field image-update state_level)"
        fi

        # 2. Nothing to do costs nothing.
        if [ "$(field normal-boot extracted)" = no ] && [ "$(field normal-boot payload_changed)" = no ] \
           && [ "$(field normal-boot rc)" = 0 ]; then
            ok "normal boot: a matching level rebuilds nothing"
        else
            bad "normal boot: something was rebuilt with nothing to do" \
                "extracted=$(field normal-boot extracted) payload_changed=$(field normal-boot payload_changed) rc=$(field normal-boot rc)"
        fi

        # 3. The short-circuit is alive, so case 1 is a real difference.
        if [ "$(field bare-match uptodate)" = yes ] && [ "$(field bare-match extracted)" = no ] \
           && [ "$(field bare-match payload_changed)" = no ]; then
            ok "control: bare rebuild at a matching level still says 'already up to date'"
        else
            bad "control: the 'already up to date' short-circuit never fired" \
                "uptodate=$(field bare-match uptodate) extracted=$(field bare-match extracted) — if it is dead, case 1 rebuilding proves nothing"
        fi

        # 4. The set leg survived the refactor.
        if [ "$(field bare-newset extracted)" = yes ] && [ "$(field bare-newset uptodate)" = no ]; then
            ok "control: a changed package set at a matching level still rebuilds"
        else
            bad "control: a changed package set did NOT rebuild" \
                "extracted=$(field bare-newset extracted) uptodate=$(field bare-newset uptodate)"
        fi

        # 6. The driver bump: same level, same set, only the image's twin moved.
        if [ "$(field driver-bump extracted)" = yes ] && [ "$(field driver-bump payload_changed)" = yes ] \
           && [ "$(field driver-bump announced)" = yes ] \
           && [ "$(field driver-bump state_twin)" = gamma-3.1-1.fc43 ]; then
            ok "driver bump: --if-needed rebuilt and recorded the image's new 32-bit twin"
        else
            bad "driver bump: --if-needed did not rebuild when the image's twin moved" \
                "extracted=$(field driver-bump extracted) announced=$(field driver-bump announced) state_twin=$(field driver-bump state_twin)"
        fi
        # 7. Once recorded, the next boot rebuilds nothing.
        if [ "$(field twin-recorded extracted)" = no ] && [ "$(field twin-recorded payload_changed)" = no ] \
           && [ "$(field twin-recorded announced)" = no ]; then
            ok "driver bump: with the new twin recorded, the next boot rebuilds nothing"
        else
            bad "driver bump: a recorded twin still triggered a rebuild" \
                "extracted=$(field twin-recorded extracted) announced=$(field twin-recorded announced) — that would rebuild on every boot"
        fi
        # 8. The mutant.
        if [ "$(field twins-mutant extracted)" = no ] && [ "$(field twins-mutant payload_changed)" = no ] \
           && [ "$(field twins-mutant announced)" = no ]; then
            ok "mutant: without the twins clause a driver bump rebuilds nothing"
        else
            bad "mutant: the engine without the twins clause still rebuilt" \
                "so the driver-bump case is not measuring that clause"
        fi

        # 5. The mutant must reproduce §0.2 exactly, including the stamped level.
        if [ "$(field mutant announced)" = yes ] && [ "$(field mutant uptodate)" = yes ] \
           && [ "$(field mutant extracted)" = no ] && [ "$(field mutant payload_changed)" = no ] \
           && [ "$(field mutant state_level)" = "$LEVEL" ]; then
            ok "mutant: announces a rebuild, builds nothing, and still stamps level ${LEVEL}"
        else
            bad "mutant: the pre-fix engine did not reproduce the defect" \
                "announced=$(field mutant announced) uptodate=$(field mutant uptodate) extracted=$(field mutant extracted) state_level=$(field mutant state_level) — a bump alone would then look like a fix, and this suite could not tell"
        fi
    fi
fi

echo
printf 'rime-pkg-update: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
