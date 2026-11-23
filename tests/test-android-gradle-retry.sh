#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  android/tools/gradle-retry.sh: reruns a Gradle command only when it failed
#  while fetching a plugin or a dependency, a bounded number of times, and
#  says "PASSED ON RETRY" when a later try passes.
#
#  The Gradle here is a fake (a script that prints recorded Gradle output and
#  exits as told, counting its runs): what is under test is the decision, and
#  the decision reads only the output and the exit code. The plugin line is
#  copied from the failed run it was written for (pr-validation 44990757311).
#
#      ./tests/test-android-gradle-retry.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOL="${GRADLE_RETRY_TOOL:-$ROOT/android/tools/gradle-retry.sh}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/rime-gradle-retry.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

pass=0 fail=0
ok() { pass=$((pass + 1)); echo "ok   $1"; }
bad() { fail=$((fail + 1)); echo "FAIL $1"; }

# Gradle's output for each kind of failure.
cat >"$WORK/plugin.txt" <<'EOF'
* What went wrong:
Plugin [id: 'org.jetbrains.kotlin.jvm', version: '2.4.20', apply: false] was not found in any of the following sources:

- Gradle Core Plugins (plugin is not in 'org.gradle' namespace)
- Included Builds (No included builds contain this plugin)
- Plugin Repositories (could not resolve plugin artifact 'org.jetbrains.kotlin.jvm:org.jetbrains.kotlin.jvm.gradle.plugin:2.4.20')
EOF
cat >"$WORK/get502.txt" <<'EOF'
> Could not resolve all files for configuration ':app:debugCompileClasspath'.
   > Could not resolve androidx.core:core-ktx:1.17.0.
      > Could not get resource 'https://dl.google.com/dl/android/maven2/androidx/core/core-ktx/1.17.0/core-ktx-1.17.0.pom'.
         > Could not GET 'https://dl.google.com/dl/android/maven2/androidx/core/core-ktx/1.17.0/core-ktx-1.17.0.pom'. Received status code 502 from server: Bad Gateway
EOF
cat >"$WORK/notfound.txt" <<'EOF'
> Could not resolve all files for configuration ':app:debugCompileClasspath'.
   > Could not find androidx.core:core-ktx:9.9.9.
     Searched in the following locations:
EOF
cat >"$WORK/testfail.txt" <<'EOF'
AddressParserTest > parses an IPv6 literal FAILED
    java.lang.AssertionError at AddressParserTest.kt:41
12 tests completed, 1 failed
> There were failing tests. See the report at: file:///x/index.html
EOF
cat >"$WORK/testfail-and-fetch.txt" <<'EOF'
12 tests completed, 1 failed
> Could not GET 'https://repo.maven.apache.org/maven2/x.pom'. Received status code 503 from server
EOF
cat >"$WORK/compile.txt" <<'EOF'
e: file:///home/runner/work/rime-os/rime-os/android/app/src/main/kotlin/A.kt:3:5 Unresolved reference 'foo'.
> Compilation error. See log for more details
EOF

# fake OUTFILE N: fails with OUTFILE's text on its first N runs, then passes.
cat >"$WORK/fake" <<'EOF'
#!/usr/bin/env bash
count="$FAKE_DIR/count"
n=$(( $(cat "$count" 2>/dev/null || echo 0) + 1 ))
echo "$n" >"$count"
if [ "$n" -le "$2" ]; then cat "$1"; exit 1; fi
echo "BUILD SUCCESSFUL"
EOF
chmod +x "$WORK/fake"

# run OUTFILE N [TRIES] -> sets rc, out, runs
run() {
    rm -f "$WORK/count"
    out="$(FAKE_DIR="$WORK" GRADLE_RETRY_DELAY=0 GRADLE_RETRY_TRIES="${3:-3}" GITHUB_ACTIONS='' \
        "$TOOL" "$WORK/fake" "$WORK/$1" "$2" 2>&1)"
    rc=$?
    runs="$(cat "$WORK/count" 2>/dev/null || echo 0)"
}

run plugin.txt 0
[ "$rc" = 0 ] && [ "$runs" = 1 ] && ! grep -q "PASSED ON RETRY" <<<"$out" \
    && ok "a pass on the first try runs once and claims no retry" || bad "first-try pass (rc=$rc runs=$runs)"

run plugin.txt 1
[ "$rc" = 0 ] && [ "$runs" = 2 ] && grep -q "PASSED ON RETRY" <<<"$out" \
    && ok "a plugin the portal did not serve is fetched again, and the pass says PASSED ON RETRY" \
    || bad "plugin retry (rc=$rc runs=$runs)"
grep -q "failed while fetching (Plugin \[id: 'org.jetbrains.kotlin.jvm'" <<<"$out" \
    && ok "the retry names the line it retried on" || bad "retry reason not named"

run get502.txt 2
[ "$rc" = 0 ] && [ "$runs" = 3 ] && ok "a 502 from a repository is retried until it passes" \
    || bad "502 retry (rc=$rc runs=$runs)"

run get502.txt 9
[ "$rc" = 1 ] && [ "$runs" = 3 ] && grep -q "giving up" <<<"$out" \
    && ok "retries are bounded: 3 tries, then Gradle's exit code" || bad "bound (rc=$rc runs=$runs)"

run get502.txt 9 1
[ "$rc" = 1 ] && [ "$runs" = 1 ] && ok "GRADLE_RETRY_TRIES=1 runs once" || bad "tries=1 (rc=$rc runs=$runs)"

for f in notfound.txt testfail.txt testfail-and-fetch.txt compile.txt; do
    run "$f" 1
    [ "$rc" = 1 ] && [ "$runs" = 1 ] && ! grep -q "again in" <<<"$out" \
        && ok "not retried: $f" || bad "retried $f (rc=$rc runs=$runs)"
done

# The exit code is Gradle's, not the wrapper's.
rm -f "$WORK/count"
FAKE_DIR="$WORK" GRADLE_RETRY_DELAY=0 "$TOOL" sh -c 'echo "BUILD FAILED"; exit 7' >/dev/null 2>&1
[ $? = 7 ] && ok "the exit code is passed through" || bad "exit code not passed through"

"$TOOL" >/dev/null 2>&1
[ $? = 2 ] && ok "no command is a usage error" || bad "no-command usage"

echo "gradle-retry: $pass passed, $fail failed"
[ "$pass" -gt 0 ] || exit 2
[ "$fail" = 0 ]
