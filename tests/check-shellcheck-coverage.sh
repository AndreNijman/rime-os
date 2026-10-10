#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  check-shellcheck-coverage.sh — every shell script is linted, by DISCOVERY.
#
#  pr-validation.yml lints a hand-written LIST of scripts. A list cannot cover
#  a file nobody remembered to add, and on 2026-09-12 it did not: 144 shell
#  scripts existed under tests/ and files/, 38 were named, and 106 were not —
#  including four suites landed in the previous two days
#  (test-rime-user.sh, test-rime-lid.sh, test-rime-lid-live.sh,
#  test-rime-permissions.sh) and both of the checkers written to catch exactly
#  this species of gap. That is the same defect as a CI path selector that
#  names nothing: the step runs, reports success, and inspects nothing.
#
#  So this file finds the scripts instead of being told them. 78 of the 106
#  were already clean; the 28 that were not are listed in
#  tests/shellcheck-known-failing.txt so the hole closes for every NEW script
#  today rather than after someone fixes 28 old ones.
#
#  Both arms fail, and the second is the one that matters:
#    * a script NOT on the list with a warning        -> fail, named
#    * a script ON the list that is now CLEAN         -> fail, "remove it"
#  Without the second, the list rots into a permanent exemption and the debt
#  it records is never paid.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 1

KNOWN=tests/shellcheck-known-failing.txt
FLAGS=(-S warning -x)

command -v shellcheck >/dev/null || { echo "FATAL: shellcheck is not installed"; exit 1; }

# Every shell script under tests/, files/, android/tools/ and kernel/. A shebang
# naming zsh or fish is NOT a shell shellcheck can read, and counting one as a
# failure would park it on the known-failing list for ever.
#
# `kernel` was added 2026-09-20 for the third time this file has had to learn
# its own lesson. Rime started building its own kernel, and kernel/ acquired
# btf-xcheck.sh — which is COPYed into the kernel build image and is one of the
# two readers that decide whether a kernel may ship — plus three spike scripts.
# Not one of them was linted by anything, because the roots were still the
# directories somebody had thought of. All four were already clean at this
# severity, so the gap cost nothing again, which is the point made below.
#
# `android/tools` was added after the same reasoning as this file's own: the
# discovery roots were the two directories somebody thought of, and the Android
# client's three gate scripts — the two that enforce its storage and colour
# rules, and the one that runs the stop-slop checker over its guide — were
# linted by nobody at all. All three were already clean at this severity, so
# the gap cost nothing this time. A root nobody listed is the same hole as a
# script nobody listed.
#
# And the same hole one level down: the exec bit was a discovery predicate. A
# script the image installs with `COPY --chmod=0755` does not need to be
# executable in the repo to be executable on the machine, so ten shipped shell
# scripts had a shebang, ran on every boot or on every NetworkManager event, and
# were linted by nobody — among them files/system/libexec/rime-env,
# rime-session-select, rime-shell-autostart and the safe-graphics autostart. All
# ten were clean at this severity, so this cost nothing either, which is the
# point: the gap is only ever free until it is not. Discovery is now the shebang
# alone. It reads the first two bytes of ~335 files and takes about a second.
mapfile -t scripts < <(
    find tests files android/tools kernel -type f 2>/dev/null \
    | while IFS= read -r f; do
        case "$f" in */__pycache__/*|*/.git/*) continue ;; esac
        if [ "${f##*.}" = sh ]; then printf '%s\n' "$f"; continue; fi
        head -c2 "$f" 2>/dev/null | pipe_has '^#!' || continue
        head -n1 "$f" | pipe_has -E '\b(sh|bash|dash|ksh)\b' && printf '%s\n' "$f"
      done | sort -u
)

known=()
[ -f "$KNOWN" ] && mapfile -t known < <(grep -vE '^\s*(#|$)' "$KNOWN")
is_known() { local n; for n in ${known[@]+"${known[@]}"}; do [ "$n" = "$1" ] && return 0; done; return 1; }

new_fail=(); fixed=(); checked=0; skipped=0
for f in ${scripts[@]+"${scripts[@]}"}; do
    checked=$((checked + 1))
    if shellcheck "${FLAGS[@]}" "$f" >/dev/null 2>&1; then
        is_known "$f" && fixed+=("$f")
    else
        is_known "$f" && skipped=$((skipped + 1)) || new_fail+=("$f")
    fi
done

printf '\nshellcheck coverage: %d scripts discovered, %d known-failing, %d newly failing, %d now clean\n' \
    "$checked" "$skipped" "${#new_fail[@]}" "${#fixed[@]}"

rc=0
if [ "${#new_fail[@]}" -gt 0 ]; then
    echo "  FAIL — these are not on $KNOWN and do not pass shellcheck ${FLAGS[*]}:"
    printf '    %s\n' "${new_fail[@]}"
    rc=1
fi
if [ "${#fixed[@]}" -gt 0 ]; then
    echo "  FAIL — these are on $KNOWN but now PASS. Delete their lines; the list must shrink:"
    printf '    %s\n' "${fixed[@]}"
    rc=1
fi
[ "$rc" -eq 0 ] && echo "  every discovered script is either clean or a recorded, still-failing exception"
exit "$rc"
