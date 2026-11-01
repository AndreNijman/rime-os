#!/bin/bash
# Fixture test for check-workflows.py (and its fork-PR dry run). No root needed.
#   bash ci/runner/tests/test-check-workflows.sh
set -uo pipefail
HERE=$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)
CHK=$HERE/check-workflows.py
F=$HERE/tests/fixtures
fails=0
expect() { # name, mode(check|sim), want(0|nonzero), pattern
	local name=$1 mode=$2 want=$3 pat=$4 out rc
	if [ "$mode" = sim ]; then out=$(python3 "$CHK" --simulate-fork-pr "$F/$name" 2>&1); else out=$(python3 "$CHK" "$F/$name" 2>&1); fi
	rc=$?
	if { [ "$want" = 0 ] && [ "$rc" = 0 ]; } || { [ "$want" != 0 ] && [ "$rc" != 0 ]; }; then
		if [ -z "$pat" ] || printf '%s\n' "$out" | grep -E "$pat" >/dev/null; then
			echo "ok   $name ($mode): rc=$rc"
			return
		fi
	fi
	echo "FAIL $name ($mode): rc=$rc, wanted $want /$pat/"; printf '%s\n' "$out" | sed 's/^/     /'
	fails=$((fails + 1))
}
expect good           check 0       'self-hosted wf.yml:nix'
expect good           sim   0       'skipped wf.yml:nix: a fork pull request does not schedule it'
expect noguard        check nonzero '^FAIL R2 wf.yml:nix'
expect noguard        sim   nonzero 'WOULD schedule it on katana'
expect prt            check nonzero '^FAIL R1 wf.yml:nix: workflow is triggered by pull_request_target'
expect signing        check nonzero 'APEX_SB_KEY_B64'
expect signing        check nonzero "signing/key material .*'sbsign'"
expect idtoken        check nonzero 'id-token: write'
expect hosted-signing check 0       ''
expect expr-label     check nonzero '^FAIL R2 wf.yml:matrix'
expect linux-label     check nonzero '^FAIL R2 wf.yml:nix'
expect upper-label     check nonzero '^FAIL R2 wf.yml:nix'
expect reusable-callee check nonzero '^FAIL R2 wf.yml:nix'
expect reusable-callee sim   nonzero 'WOULD schedule it on katana'
expect reusable-prt    check nonzero '^FAIL R1 caller.yml:call: pull_request_target workflow calls'
expect reusable-prt-hosted check 0 ''
# The repository's own workflows must pass both.
expect ../../../../.github/workflows check 0 'checked [0-9]+ workflow file'
expect ../../../../.github/workflows sim   0 'fork-PR dry run: 0 self-hosted'
echo "RESULT failures=$fails"
exit "$fails"
