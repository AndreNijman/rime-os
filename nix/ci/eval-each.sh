#!/usr/bin/env bash
# Evaluate every flake output ONE PROCESS PER ATTRIBUTE, so a hosted runner's 16 GB is enough.
#
# Why: `nix flake check --no-build` evaluates every NixOS test in one evaluator. With the wave 2 checks that peaked
# at 27.9 GB RSS (measured 2026-11-02 in the rime-nix container, 15 min wall clock under load) and the hosted
# ubuntu-24.04 runner (16 GB) killed it (exit 143) about 2.5 min in. Each `nix eval` here frees its heap on exit.
#
# usage: nix/ci/eval-each.sh FLAKE [extra nix args, e.g. --override-input rime-shell github:...]
# env:   RIME_EVAL_JOBS  attributes evaluated at once (default 2)
#        RIME_EVAL_HEAVY_MIB  an attribute NOT in nix/ci/eval-heavy.txt that peaks above this prints a warning
#                       telling you to add it there (default 4096)
#        SYSTEM          default x86_64-linux
#        RIME_EVAL_OUT   write "<attr><TAB><value>" for every attribute that evaluated (the drvPaths) to this file:
#                       the plan job's input for reusing checks that passed earlier (run-checks.sh plan, RIME_CHECK_DRVS)
# Prints one line per attribute: OK|FAIL <attr> <seconds>s <max RSS MiB>. Exit 1 if any attribute fails.
set -uo pipefail

FLAKE="${1:?usage: eval-each.sh FLAKE [nix args...]}"
shift
SYSTEM="${SYSTEM:-x86_64-linux}"
JOBS="${RIME_EVAL_JOBS:-2}"
HEAVY_MIB="${RIME_EVAL_HEAVY_MIB:-4096}"
# Start a light evaluation only with this much free: above the biggest light peak (update-floors, 3.8 GB, 2026-11-04).
GATE="${RIME_EVAL_GATE_MIB:-5120}"
# Attributes measured too big to share a 16 GB hosted runner with others (peak RSS per evaluator, hosted run
# 41687259044 and the rime-nix container, 2026-11-04): evaluated ONE AT A TIME after the parallel batch.
HEAVY_LIST="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/eval-heavy.txt"
NIX_ARGS=("$@")

names() { # names OUTPUT-PATH -> attribute names, one per line
    nix eval --raw "${NIX_ARGS[@]}" "$FLAKE#$1" --apply 'a: builtins.concatStringsSep "\n" (builtins.attrNames a)'
    echo
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/val"

{
    for n in $(names "checks.$SYSTEM"); do echo "checks.$SYSTEM.$n.drvPath"; done
    for n in $(names "packages.$SYSTEM"); do echo "packages.$SYSTEM.$n.drvPath"; done
    for n in $(names nixosConfigurations); do echo "nixosConfigurations.$n.config.system.build.toplevel.drvPath"; done
} > "$work/attrs"
[ -s "$work/attrs" ] || { echo "eval-each: no attributes found in $FLAKE" >&2; exit 1; }

# Modules and overlays: evaluating them means applying them, which the configurations above already did; here
# only that they exist and are functions or attrsets (what `nix flake check` asserts for them).
for out in nixosModules overlays; do
    if ! nix eval "${NIX_ARGS[@]}" "$FLAKE#$out" --apply \
        'm: builtins.all (v: builtins.isFunction v || builtins.isAttrs v || builtins.isPath v) (builtins.attrValues m)' \
        2>"$work/$out.err" | grep -x true >/dev/null; then
        echo "FAIL $out"; cat "$work/$out.err" >&2; echo 1 > "$work/failed"
    fi
done

# The kB value of one /proc key ("VmHWM:", "MemAvailable:"). Plain bash: the rime-nix container has no awk or sed.
field() {
    local k v _
    while read -r k v _; do [ "$k" = "$1" ] && { echo "$v"; return; }; done < "$2" 2>/dev/null
}
export -f field

one() {
    local attr="$1" t0 rc rss
    # Light attributes run $JOBS at a time: start one only while GATE_MIB is free, so a few ~3 GB evaluators
    # finishing late cannot stack up on a 16 GB runner. The heavy pass runs one at a time with GATE_MIB=0.
    while [ "${GATE_MIB:-0}" -gt 0 ] && [ "$(( $(field MemAvailable: /proc/meminfo) / 1024 ))" -lt "$GATE_MIB" ]; do
        sleep 2
    done
    t0=$(date +%s)
    # GNU time is not on every runner: poll the evaluator's own peak RSS (VmHWM) while it runs.
    nix eval --raw "${NIX_ARGS[@]}" "$FLAKE#$attr" >"$work/val/${attr//\//_}" 2>"$work/${attr//\//_}.err" &
    local pid=$! peak=0 cur
    while kill -0 "$pid" 2>/dev/null; do
        cur=$(field VmHWM: "/proc/$pid/status")
        [ "${cur:-0}" -gt "$peak" ] && peak=$cur
        sleep 0.5
    done
    wait "$pid"; rc=$?
    rss=$((peak / 1024))
    if [ "$rc" = 0 ]; then
        echo "OK $attr $(( $(date +%s) - t0 ))s ${rss}MiB"
        if [ "$rss" -gt "$HEAVY_MIB" ] && ! grep -qxF "${attr%.drvPath}" "$work/heavy" 2>/dev/null; then
            echo "::warning::eval-each: ${attr%.drvPath} peaked at ${rss} MiB; add it to nix/ci/eval-heavy.txt so it is evaluated alone"
        fi
    else
        echo "FAIL $attr $(( $(date +%s) - t0 ))s ${rss}MiB"
        tail -n 40 "$work/${attr//\//_}.err" | while IFS= read -r l; do printf '  %s\n' "$l"; done
        echo 1 > "$work/failed"
    fi
}
export -f one
export FLAKE work HEAVY_MIB
export NIX_ARGS_STR="${NIX_ARGS[*]-}"

# Split off the heavy attributes. Plain bash, no sed/awk: the rime-nix container has neither.
: > "$work/heavy"
if [ -r "$HEAVY_LIST" ]; then
    while read -r line; do
        line="${line%%#*}"; line="${line//[[:space:]]/}"
        [ -n "$line" ] && printf '%s\n' "${line//x86_64-linux/$SYSTEM}" >> "$work/heavy"
    done < "$HEAVY_LIST"
fi
: > "$work/light"; : > "$work/alone"
while read -r attr; do
    if grep -qxF "${attr%.drvPath}" "$work/heavy"; then echo "$attr" >> "$work/alone"; else echo "$attr" >> "$work/light"; fi
done < "$work/attrs"

# xargs runs `one` in a fresh bash: hand it the extra args as a plain string (they never contain spaces of their own).
# shellcheck disable=SC2016 # expanded by the inner bash, on purpose
GATE_MIB="$GATE" xargs -P "$JOBS" -I{} bash -c 'NIX_ARGS=($NIX_ARGS_STR); one "$@"' _ {} < "$work/light"
# shellcheck disable=SC2016 # expanded by the inner bash, on purpose
GATE_MIB=0 xargs -P 1 -I{} bash -c 'NIX_ARGS=($NIX_ARGS_STR); one "$@"' _ {} < "$work/alone"

if [ -n "${RIME_EVAL_OUT:-}" ]; then
    : > "$RIME_EVAL_OUT"
    while read -r attr; do
        f="$work/val/${attr//\//_}"
        [ -s "$f" ] && printf '%s\t%s\n' "$attr" "$(cat "$f")" >> "$RIME_EVAL_OUT"
    done < "$work/attrs"
fi
echo "eval-each: $(wc -l < "$work/attrs") attributes evaluated, $JOBS at a time; $(wc -l < "$work/alone") heavy ones alone"
[ ! -e "$work/failed" ]
