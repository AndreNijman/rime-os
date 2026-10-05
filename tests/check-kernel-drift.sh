#!/usr/bin/env bash
# check-kernel-drift.sh — has the world moved away from kernel/kernel.pin?
#
# WHY THIS EXISTS
#
# Building our own kernel changed the shape of the security-update obligation,
# and not for the better. Before: the COPR rebuilt kernel-cachyos when CachyOS
# tagged a release, and Rime picked it up with a forced core rebuild. Nothing
# had to be remembered. Now: KERNEL_TAG and its sha256 in kernel/kernel.pin are
# what decide which kernel Rime ships, and they change when a human changes
# them. An unbumped pin is a kernel that quietly stops receiving security fixes
# WHILE EVERY GATE IN THIS REPOSITORY STAYS GREEN -- the build still verifies
# its sha256, the BTF gate still passes, CI is still all ticks. Green means
# "this is the kernel you pinned", never "this kernel is current".
#
# So the pin is a promise to keep bumping it, and this is the thing that
# notices when nobody has. It is the mechanism that stops the obligation from
# being something a person has to remember.
#
# THREE OUTCOMES, NOT TWO
#
# The trap this repository has paid for repeatedly is a failed lookup reported
# as a checked fact -- a rate-limited API answering 403 and the script
# concluding "no newer kernel". So a lookup that cannot be performed is its own
# outcome and its own non-zero exit, and it says which lookup failed:
#
#   0  NO DRIFT      every input checked, every one current
#   1  DRIFT         something upstream moved; the report names what
#   2  COULD NOT TELL  a lookup failed. NOT a pass. NOT drift.
#
# Run it by hand any time; `.github/workflows/kernel-drift.yml` runs it weekly.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PIN="${ROOT}/kernel/kernel.pin"

drift=0
unknown=0

drifted() { echo "DRIFT:   $*"; drift=$((drift + 1)); }
unknowable() { echo "UNKNOWN: $*"; unknown=$((unknown + 1)); }
current() { echo "current: $*"; }

[ -s "${PIN}" ] || { echo "UNKNOWN: no ${PIN}"; exit 2; }
set -a
# The directive has to sit directly above the `.` and nothing else: written as
# `set -a; . "${PIN}"; set +a` on one line it attaches to `set -a`, leaves
# SC1090 live, and fails tests/check-shellcheck-coverage.sh.
# shellcheck source=/dev/null
. "${PIN}"
set +a

echo "kernel/kernel.pin: KERNEL_TAG=${KERNEL_TAG} DWARVES_NVR=${DWARVES_NVR}"
echo

# `curl -f` so an HTML error page is a failure rather than a body to parse, and
# the exit status is tested explicitly -- a 403 body fed to a JSON parser is how
# "no newer tag" gets concluded from a rate limit.
fetch() {
    curl -fsSL --max-time 45 --retry 2 --retry-delay 3 "$@" 2>/dev/null
}

# ── 1. has CachyOS tagged a newer kernel? ───────────────────────────────────
# This is the security-update question. CachyOS rebases on upstream stable, so
# a newer cachyos-7.2.N-M tag is where the CVE fixes are.
#
# `git ls-remote`, NOT the GitHub tags API. Measured 2026-09-20: the API's
# /tags endpoint returns tags in an order of its own, and page one of
# CachyOS/linux is 100 tags of `v5.18-rc*` with not a single `cachyos-*` among
# them -- so a one-page read finds nothing and, written the obvious way, calls
# that "no newer kernel". ls-remote returns all 148 in 1.1 s with no API token
# and no rate limit.
if ! tags="$(git ls-remote --tags --refs https://github.com/CachyOS/linux 'cachyos-*' 2>/dev/null)" \
     || [ -z "${tags}" ]; then
    unknowable "could not list CachyOS/linux tags (network, or the repo moved)."
    echo "         This is NOT 'no newer kernel'."
else
    # -rc tags are excluded. cachyos-7.3-rc3-4 sorts above every 7.2.x and is a
    # PRERELEASE: reporting it as a missed security update would train whoever
    # reads this to ignore it.
    names="$(printf '%s\n' "${tags}" | sed 's|.*refs/tags/||' | grep -v -- '-rc' | sort -V)"
    n_tags="$(printf '%s\n' "${names}" | grep -c .)"
    TAGVER="${KERNEL_TAG#cachyos-}"                 # 7.2.6-1
    SERIES="$(printf '%s' "${TAGVER}" | cut -d. -f1,2)"   # 7.2

    # Two questions, deliberately separate. A newer 7.2.x is a security update
    # and is urgent. A newer 7.3 is a kernel upgrade and is a decision.
    newest_series="$(printf '%s\n' "${names}" | grep -E "^cachyos-${SERIES}\." | sort -V | tail -1)"
    newest_any="$(printf '%s\n' "${names}" | sort -V | tail -1)"

    if [ -z "${newest_series}" ]; then
        unknowable "no cachyos-${SERIES}.* tag exists upstream at all; is KERNEL_TAG=${KERNEL_TAG} real?"
    elif [ "${newest_series}" != "${KERNEL_TAG}" ]; then
        drifted "CachyOS has tagged ${newest_series} in the ${SERIES} series; kernel.pin is on ${KERNEL_TAG}."
        echo "         THIS IS THE SECURITY UPDATE. Bump KERNEL_TAG, KERNEL_SRC_URL and"
        echo "         KERNEL_SRC_SHA256. Until that happens Rime ships a kernel that"
        echo "         receives no security fixes, and every gate stays green while it does."
    else
        current "KERNEL_TAG=${KERNEL_TAG} is the newest stable tag in the ${SERIES} series (of ${n_tags} stable tags)"
    fi

    if [ -n "${newest_any}" ] && [ "${newest_any}" != "${newest_series}" ]; then
        echo "note:    CachyOS has a newer stable series: ${newest_any}. Moving to it is a"
        echo "         kernel upgrade and a decision, not a security fix — not reported as drift."
    fi
fi

# ── 2. is the pinned dwarves still what Fedora has? ─────────────────────────
# 1.32 was in updates-testing when it was pinned, which is why kernel.pin uses
# koji NVR URLs rather than `dnf install dwarves`.
#
# BODHI, NOT MDAPI, and this is the whole point of the section. The obvious
# source, apps.fedoraproject.org/mdapi/f43/pkg/dwarves, answers `version:
# 1.32` -- and three fields later, `repo: updates-testing`. Read without that
# qualifier it says 1.32 is in f43, and a drift watcher built on it reports
# "1.32 has reached stable, you can relax the pin" while bodhi says
# date_stable=None. That false positive was live in this script until it was
# checked against a second source. Bodhi carries the status as the answer
# rather than as a footnote.
DW_NVR="dwarves-${DWARVES_NVR}"
# The Fedora release the pin was built for, from its own dist tag
# (1.32-1.fc45 -> F45), so this asks bodhi about the release the kernel is
# actually built on rather than one written down here.
DW_REL="F$(printf '%s' "${DWARVES_NVR}" | sed -n 's/.*\.fc\([0-9][0-9]*\).*/\1/p')"
bodhi="$(fetch -H 'Accept: application/json' \
    "https://bodhi.fedoraproject.org/updates/?packages=dwarves&releases=${DW_REL}&rows_per_page=20")"
if [ -z "${bodhi}" ]; then
    unknowable "could not ask bodhi about dwarves in ${DW_REL}"
else
    read -r dw_status dw_stable_nvr <<<"$(printf '%s' "${bodhi}" | DW_NVR="${DW_NVR}" python3 -c '
import json, os, sys
try:
    ups = json.load(sys.stdin).get("updates", [])
except Exception:
    sys.exit(3)
want = os.environ["DW_NVR"]
status, newest_stable = "", ""
for u in ups:
    title = u.get("title", "")
    if title == want:
        status = u.get("status", "") or "?"
        # date_stable is the fact; status can read "testing" while it is on its
        # way in, so both are consulted.
        if u.get("date_stable"):
            status = "stable"
    if u.get("status") == "stable" and not newest_stable:
        newest_stable = title
print(status or "-", newest_stable or "-")
')"
    if [ -z "${dw_status}" ] || [ "${dw_status}" = "-" ]; then
        unknowable "bodhi lists no ${DW_REL} update called ${DW_NVR}; cannot tell whether it is still testing, or was withdrawn"
    else
        case "${dw_status}" in
            # Stable is CURRENT, not drift. This used to be reported as drift
            # so a person would relax the pin to 'dwarves >= 1.32' once it got
            # there. It got there on 2026-09-25, and the pin was KEPT instead:
            # a koji NVR with a sha256 is content-addressed, a dnf requirement
            # resolves against the live repository on the day of the build. So
            # the answer to "does the pinned dwarves still exist in a sane
            # state" is yes, and the koji URL check below keeps asking.
            stable)
                current "${DW_NVR} is in ${DW_REL} stable; kept pinned by koji NVR + sha256 on purpose (content-addressed)" ;;
            testing|pending)
                current "${DW_NVR} is still '${dw_status}' in ${DW_REL} (stable is ${dw_stable_nvr}); the koji NVR pin is still required" ;;
            unpushed|obsolete|revoked)
                drifted "${DW_NVR} was ${dw_status} from ${DW_REL} updates-testing."
                echo "         Fedora withdrew it. The koji URL still resolves, so the build"
                echo "         keeps working and NOTHING ELSE HERE WOULD NOTICE. Find out why"
                echo "         it was withdrawn before the next kernel bump. The BTF gate is"
                echo "         the protection if the reason was a regression — a bad 1.32"
                echo "         shows up as the build refusing its own kernel." ;;
            *)
                unknowable "${DW_NVR} has bodhi status '${dw_status}', which this does not know how to read" ;;
        esac
    fi
fi

# ── 3. do the pinned artefacts still exist? ─────────────────────────────────
# An update in updates-testing can be UNPUSHED. Koji keeps a built package's
# URL valid, so this is expected to stay green -- which is the point. If it ever
# does not, the pin is naming something that no longer exists and the next
# build fails at the download rather than at anything informative.
for pair in "DWARVES_RPM_URL=${DWARVES_RPM_URL}" \
            "LIBDWARVES_RPM_URL=${LIBDWARVES_RPM_URL}" \
            "KERNEL_SRC_URL=${KERNEL_SRC_URL}" \
            "KERNEL_CONFIG_URL=${KERNEL_CONFIG_URL}" \
            "KERNEL_PATCH_BORE_URL=${KERNEL_PATCH_BORE_URL}"; do
    key="${pair%%=*}"; url="${pair#*=}"
    code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 45 -I -L "${url}" 2>/dev/null)"
    case "${code}" in
        200) current "${key} still resolves" ;;
        # GitHub's codeload answers HEAD on an archive with 403 while GET works.
        403|405)
            code2="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 60 -L -r 0-0 "${url}" 2>/dev/null)"
            case "${code2}" in
                200|206) current "${key} still resolves (HEAD refused ${code}, ranged GET ${code2})" ;;
                *) unknowable "${key} answered ${code} to HEAD and ${code2} to a ranged GET" ;;
            esac ;;
        404|410) drifted "${key} is GONE (${code}): ${url}" ;;
        000) unknowable "${key} could not be reached at all" ;;
        *) unknowable "${key} answered ${code}" ;;
    esac
done

echo
if [ "${unknown}" -gt 0 ]; then
    echo "kernel drift: ${unknown} lookup(s) could not be performed, ${drift} drift(s) found."
    echo "COULD NOT TELL — this is not a pass. Fix the lookup and run it again."
    exit 2
fi
if [ "${drift}" -gt 0 ]; then
    echo "kernel drift: ${drift} item(s) moved. kernel/kernel.pin needs a human."
    exit 1
fi
echo "kernel drift: none. Every pinned input is current and still resolves."
exit 0
