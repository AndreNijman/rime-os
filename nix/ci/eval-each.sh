#!/usr/bin/env bash
# Evaluate every flake output ONE PROCESS PER ATTRIBUTE, so a hosted runner's 16 GB is enough.
#
# Why: `nix flake check --no-build` evaluates every NixOS test in one evaluator. With the wave 2 checks that peaked
# at 27.9 GB RSS (measured 2026-11-02 in the rime-nix container, 15 min wall clock under load) and the hosted
# ubuntu-24.04 runner (16 GB) killed it (exit 143) about 2.5 min in. Each `nix eval` here frees its heap on exit.
#
# usage: nix/ci/eval-each.sh FLAKE [extra nix args, e.g. --override-input rime-shell github:...]
# env:   RIME_EVAL_JOBS  attributes evaluated at once (default 2)
#        SYSTEM          default x86_64-linux
# Prints one line per attribute: OK|FAIL <attr> <seconds>s <max RSS MiB>. Exit 1 if any attribute fails.
set -uo pipefail

FLAKE="${1:?usage: eval-each.sh FLAKE [nix args...]}"
shift
SYSTEM="${SYSTEM:-x86_64-linux}"
JOBS="${RIME_EVAL_JOBS:-2}"
NIX_ARGS=("$@")

names() { # names OUTPUT-PATH -> attribute names, one per line
    nix eval --raw "${NIX_ARGS[@]}" "$FLAKE#$1" --apply 'a: builtins.concatStringsSep "\n" (builtins.attrNames a)'
    echo
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

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

one() {
    local attr="$1" t0 rc rss
    t0=$(date +%s)
    # GNU time is not on every runner: poll the evaluator's own peak RSS (VmHWM) while it runs.
    nix eval --raw "${NIX_ARGS[@]}" "$FLAKE#$attr" >/dev/null 2>"$work/${attr//\//_}.err" &
    local pid=$! peak=0 cur
    while kill -0 "$pid" 2>/dev/null; do
        cur=$(awk '/^VmHWM:/ {print $2}' "/proc/$pid/status" 2>/dev/null)
        [ "${cur:-0}" -gt "$peak" ] && peak=$cur
        sleep 0.5
    done
    wait "$pid"; rc=$?
    rss=$((peak / 1024))
    if [ "$rc" = 0 ]; then
        echo "OK $attr $(( $(date +%s) - t0 ))s ${rss}MiB"
    else
        echo "FAIL $attr $(( $(date +%s) - t0 ))s ${rss}MiB"
        sed 's/^/  /' "$work/${attr//\//_}.err" | tail -n 40
        echo 1 > "$work/failed"
    fi
}
export -f one
export FLAKE work
export NIX_ARGS_STR="${NIX_ARGS[*]-}"

# xargs runs `one` in a fresh bash: hand it the extra args as a plain string (they never contain spaces of their own).
# shellcheck disable=SC2016 # expanded by the inner bash, on purpose
xargs -P "$JOBS" -I{} bash -c 'NIX_ARGS=($NIX_ARGS_STR); one "$@"' _ {} < "$work/attrs"

echo "eval-each: $(wc -l < "$work/attrs") attributes evaluated, $JOBS at a time"
[ ! -e "$work/failed" ]
