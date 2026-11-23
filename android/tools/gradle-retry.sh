#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  Run a Gradle command; run it again, after a backoff, ONLY when it failed
#  while fetching a plugin or a dependency.
#
#      android/tools/gradle-retry.sh ./gradlew --no-daemon :core:test
#
#  Why: pr-validation run 44990757311 (attempt 1) failed in the Android job on
#  `Plugin [id: 'org.jetbrains.kotlin.jvm', version: '2.4.20'] was not found
#  ... could not resolve plugin artifact`, a fetch from the plugin portal that
#  answered on the next attempt. The job is cold on every new PR (its Gradle
#  cache is saved per PR, and pr-validation does not run on main), so every
#  run downloads the plugins and dependencies again, and one bad answer cost
#  a whole re-run of the required check.
#
#  What it retries, and nothing else: Gradle's own resolution failures
#  ("Could not resolve g:a:v", "Could not GET/HEAD/download ...", "Plugin [...]
#  was not found", a 429/5xx status from a repository). A compile error, a
#  failing test, an artifact that is not there ("Could not find g:a:v"), a lint
#  error or anything it does not recognise fails at once with Gradle's exit
#  code, also in a run where a fetch failed too. Gradle verifies what it
#  downloads, and a fresh try changes nothing about that.
#
#  A pass after a failed try is printed as "PASSED ON RETRY", so a flaky
#  repository stays visible in the log instead of disappearing.
#
#  GRADLE_RETRY_TRIES (default 3) and GRADLE_RETRY_DELAY (seconds before the
#  2nd try, doubled after; default 30) exist for the suite.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

[ "$#" -gt 0 ] || { echo "gradle-retry: usage: gradle-retry.sh COMMAND [ARG...]" >&2; exit 2; }
tries="${GRADLE_RETRY_TRIES:-3}"
delay="${GRADLE_RETRY_DELAY:-30}"
[[ "$tries" =~ ^[1-9][0-9]*$ && "$delay" =~ ^[0-9]+$ ]] || {
    echo "gradle-retry: GRADLE_RETRY_TRIES must be >= 1 and GRADLE_RETRY_DELAY a whole number" >&2; exit 2; }

annot() { [ "${GITHUB_ACTIONS:-}" = true ] && printf '::%s::' "$1"; return 0; }

# The resolution failure that makes a try worth repeating, or nothing.
fetch_failure() {
    local log="$1"
    # A test that failed, a compile error, or an artifact the repositories
    # answered "not there" for (Gradle's "Could not find g:a:v", a 404) is a
    # real failure even when a fetch also failed in the same run.
    if grep -Eq '[0-9]+ tests? completed, [0-9]+ failed|There were failing tests|Compilation error|^e: |Could not find [A-Za-z0-9_.-]+:' "$log"; then
        return 1
    fi
    # The plugin line cannot tell a portal that did not answer from a version
    # that does not exist (Gradle prints the same words, without the cause);
    # a real typo costs the backoff, then fails with Gradle's own message.
    grep -E -m1 \
        -e "Plugin \[id: '[^']+'(, version: '[^']+')?(, apply: (true|false))?\] was not found" \
        -e 'Could not resolve (plugin artifact|[A-Za-z0-9_.-]+:[A-Za-z0-9_.-]+)' \
        -e "Could not (GET|HEAD|download) " \
        -e 'Received status code (429|5[0-9][0-9]) from server' \
        "$log"
}

log="$(mktemp "${TMPDIR:-/tmp}/gradle-retry.XXXXXX")"
trap 'rm -f "$log"' EXIT

n=1
while :; do
    "$@" 2>&1 | tee "$log"
    rc="${PIPESTATUS[0]}"
    if [ "$rc" -eq 0 ]; then
        if [ "$n" -gt 1 ]; then
            echo "$(annot warning)gradle-retry: PASSED ON RETRY: try $n of $tries passed after a failed fetch: $*"
        fi
        exit 0
    fi
    if ! why="$(fetch_failure "$log")"; then
        [ "$n" -gt 1 ] && echo "gradle-retry: try $n of $tries failed, not while fetching a plugin or dependency"
        exit "$rc"
    fi
    why="$(sed -E 's/^[[:space:]>]+//' <<<"$why" | cut -c1-200)"
    if [ "$n" -ge "$tries" ]; then
        echo "$(annot error)gradle-retry: still failing to fetch after $tries tries ($why); giving up"
        exit "$rc"
    fi
    wait=$(( delay * (1 << (n - 1)) ))
    echo "$(annot warning)gradle-retry: try $n of $tries failed while fetching ($why); the same command again in ${wait} s"
    sleep "$wait"
    n=$(( n + 1 ))
done
