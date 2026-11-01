#!/bin/bash
# Job container entrypoint. Runs as the unprivileged `runner` user.
#
# Normal mode: the host wrapper (rime-runner-job) writes ONE line on stdin, the
# base64 just-in-time config GitHub minted for this container. It is read here,
# never written to disk on the host and never put in the environment, and the
# runner is started with stdin closed. A JIT runner takes exactly one job and
# GitHub removes its registration when that job ends.
#
# Dry-run mode (RIME_RUNNER_DRY_RUN=1): nothing is registered; run the isolation
# probes instead and exit with their failure count.
set -euo pipefail

if [ "${RIME_RUNNER_DRY_RUN:-0}" = 1 ]; then
	exec /usr/local/libexec/rime-runner/probe.sh
fi

IFS= read -r jit || true
[ -n "${jit:-}" ] || { echo "rime-runner: no JIT config on stdin" >&2; exit 64; }
exec /opt/actions-runner/run.sh --jitconfig "$jit" </dev/null
