#!/usr/bin/env bash
# Evaluate every flake output ONE PROCESS PER ATTRIBUTE, so a hosted runner's 16 GB is enough; optionally only one
# shard of them, so several runners share the work (nix.yml's eval matrix).
#
# Why one process each: `nix flake check --no-build` evaluates every NixOS test in one evaluator. With the wave 2
# checks that peaked at 27.9 GB RSS (measured 2026-11-02 in the rime-nix container, 15 min wall clock under load) and
# the hosted ubuntu-24.04 runner (16 GB) killed it (exit 143) about 2.5 min in. Each `nix eval` here frees its heap
# on exit.
#
# Why shards: one runner took 1500-1517 s for the 131 items (hosted Nix runs, 2026-11-08..22), and every check leg
# waited for it. The work is ~3,700 evaluator-seconds, ~710 of them in the heavy items that must run alone.
#
# usage: nix/ci/eval-each.sh [--shard K/N] FLAKE [extra nix args, e.g. --override-input rime-shell github:...]
#        nix/ci/eval-each.sh --list FLAKE [nix args]      every item, one per line (what a shard and --merge enumerate)
#        nix/ci/eval-each.sh --shards N FLAKE [nix args]  which shard evaluates which item, with each shard's estimate
#        nix/ci/eval-each.sh --merge PARTS FLAKE [nix args]
#                       PARTS holds one directory per shard (each shard's RIME_EVAL_PART). Refuses unless there are
#                       exactly N parts numbered 1..N, every part enumerated the same items as this run does, every
#                       item was assigned to exactly ONE shard, and every item evaluated OK with a value. Writes the
#                       merged RIME_EVAL_OUT (required). It never decides from a shard's exit status: a shard that
#                       swallowed a failure still lacks the value, and the merge refuses it.
#
# Items: checks.SYSTEM.*.drvPath, packages.SYSTEM.*.drvPath, nixosConfigurations.*.config.system.build.toplevel
# .drvPath, plus nixosModules and overlays (each must be an attrset of functions/attrsets/paths, what `nix flake
# check` asserts for them).
#
# The partition (--shard, --shards) is a pure function of the item list, N, nix/ci/eval-cost.tsv and
# nix/ci/eval-heavy.txt: greedy longest-first onto the least loaded shard. The cost table changes only the BALANCE,
# never membership: an item it does not name gets RIME_EVAL_DEFAULT_SECONDS. A heavy item counts 4x (it runs alone;
# light ones run 4 at a time on a 4-core runner), so a shard's estimate is its weight / 4 in seconds.
#
# env:   RIME_EVAL_JOBS  attributes evaluated at once (default 2)
#        RIME_EVAL_HEAVY_MIB  an attribute NOT in nix/ci/eval-heavy.txt that peaks above this prints a warning
#                       telling you to add it there (default 4096)
#        SYSTEM          default x86_64-linux
#        RIME_EVAL_OUT   write "<attr><TAB><value>[<TAB><first outPath>]" for every item that evaluated (the drvPaths,
#                       and the output path a .drv names, resolved on the machine that instantiated it): the plan
#                       job's input for reusing checks that passed earlier (run-checks.sh plan, RIME_CHECK_DRVS)
#        RIME_EVAL_PART  a directory: this shard's part for --merge (shard = "K N", all = every item, assigned =
#                       this shard's items, status = "OK|FAIL <item>", drvs.tsv = as RIME_EVAL_OUT)
#        RIME_EVAL_ATTRS_FILE  read the item list from this file instead of the flake (the tests)
#        RIME_EVAL_COST  the cost table (default nix/ci/eval-cost.tsv)
#        RIME_EVAL_DEFAULT_SECONDS  the weight of an item the cost table does not name (default 45)
# Prints one line per attribute: OK|FAIL <attr> <seconds>s <max RSS MiB>. Exit 1 if any attribute fails.
set -uo pipefail

CI_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODE=run SHARD=1 SHARDS=1 PARTS=''
case "${1:-}" in
    --shard)
        spec="${2:?usage: eval-each.sh --shard K/N FLAKE [nix args...]}"; shift 2
        SHARD="${spec%%/*}"; SHARDS="${spec#*/}"
        ;;
    --list) MODE=list; shift ;;
    --shards) MODE=shards; SHARDS="${2:?usage: eval-each.sh --shards N FLAKE}"; shift 2 ;;
    --merge) MODE=merge; PARTS="${2:?usage: eval-each.sh --merge PARTS FLAKE}"; shift 2 ;;
esac
for v in "$SHARD" "$SHARDS"; do
    case "$v" in
        '' | *[!0-9]*) echo "eval-each: --shard wants K/N in whole numbers (N for --shards), got '$SHARD/$SHARDS'" >&2; exit 2 ;;
    esac
done
SHARD=$((10#$SHARD)) SHARDS=$((10#$SHARDS))
if [ "$SHARDS" -lt 1 ] || [ "$SHARD" -lt 1 ] || [ "$SHARD" -gt "$SHARDS" ]; then
    echo "eval-each: shard $SHARD of $SHARDS does not exist (want 1 <= K <= N)" >&2; exit 2
fi

FLAKE="${1:?usage: eval-each.sh [--shard K/N] FLAKE [nix args...]}"
shift
SYSTEM="${SYSTEM:-x86_64-linux}"
JOBS="${RIME_EVAL_JOBS:-2}"
HEAVY_MIB="${RIME_EVAL_HEAVY_MIB:-4096}"
# Start a light evaluation only with this much free: above the biggest light peak (update-floors, 3.8 GB, 2026-11-04).
GATE="${RIME_EVAL_GATE_MIB:-5120}"
# Attributes measured too big to share a 16 GB hosted runner with others (peak RSS per evaluator, hosted run
# 41687259044 and the rime-nix container, 2026-11-04): evaluated ONE AT A TIME after the parallel batch.
HEAVY_LIST="$CI_DIR/eval-heavy.txt"
COST_TABLE="${RIME_EVAL_COST:-$CI_DIR/eval-cost.tsv}"
DEFAULT_SECONDS="${RIME_EVAL_DEFAULT_SECONDS:-45}"
# The partition's model of a hosted runner: light items 4 at a time. A constant, NOT RIME_EVAL_JOBS, so a local run
# with other parallelism assigns exactly what CI assigns.
MODEL_JOBS=4
NIX_ARGS=("$@")

names() { # names OUTPUT-PATH -> attribute names, one per line
    nix eval --raw "${NIX_ARGS[@]}" "$FLAKE#$1" --apply 'a: builtins.concatStringsSep "\n" (builtins.attrNames a)'
    echo
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/val" "$work/out" "$work/st"

# Every item, one per line, into $1. A listing that fails is an error, never a shorter list.
enumerate() {
    local into=$1 kind n list
    if [ -n "${RIME_EVAL_ATTRS_FILE:-}" ]; then
        grep -vE '^[[:space:]]*(#|$)' "$RIME_EVAL_ATTRS_FILE" > "$into" || true
    else
        : > "$into"
        for kind in "checks.$SYSTEM" "packages.$SYSTEM" nixosConfigurations; do
            list=$(names "$kind") || { echo "eval-each: cannot list $kind in $FLAKE" >&2; return 1; }
            for n in $list; do
                case "$kind" in
                    nixosConfigurations) echo "nixosConfigurations.$n.config.system.build.toplevel.drvPath" ;;
                    *) echo "$kind.$n.drvPath" ;;
                esac
            done >> "$into"
        done
        printf '%s\n' nixosModules overlays >> "$into"
    fi
    [ -s "$into" ] || { echo "eval-each: no attributes found in $FLAKE" >&2; return 1; }
}

# The heavy list, with SYSTEM substituted. Plain bash, no sed/awk: the rime-nix container has neither.
: > "$work/heavy"
if [ -r "$HEAVY_LIST" ]; then
    while read -r line; do
        line="${line%%#*}"; line="${line//[[:space:]]/}"
        [ -n "$line" ] && printf '%s\n' "${line//x86_64-linux/$SYSTEM}" >> "$work/heavy"
    done < "$HEAVY_LIST"
fi

# assign ITEMS N -> "K<TAB>item" for every item, in item order. Greedy longest-first onto the least loaded shard
# (ties: the lower shard), weights from the cost table. Also writes $work/load.K (each shard's weight).
assign() {
    local items=$1 n=$2 a s _ key w k best item
    declare -A sec=() heavy=() owner=()
    if [ -r "$COST_TABLE" ]; then
        while IFS=$'\t' read -r a s _; do
            case "$a" in '' | '#'*) continue ;; esac
            sec["${a//x86_64-linux/$SYSTEM}"]=$s
        done < "$COST_TABLE"
    fi
    while read -r a; do heavy["$a"]=1; done < "$work/heavy"
    local -a load=()
    for ((k = 1; k <= n; k++)); do load[k]=0; done
    while read -r item; do
        key=${item%.drvPath}
        w=${sec[$key]:-$DEFAULT_SECONDS}
        [ -n "${heavy[$key]:-}" ] && w=$((w * MODEL_JOBS))
        printf '%s\t%s\n' "$w" "$item"
    done < "$items" | LC_ALL=C sort -t "$(printf '\t')" -k1,1nr -k2,2 > "$work/weighted"
    while IFS=$'\t' read -r w item; do
        best=1
        for ((k = 2; k <= n; k++)); do
            [ "${load[k]}" -lt "${load[best]}" ] && best=$k
        done
        load[best]=$((load[best] + w))
        owner["$item"]=$best
    done < "$work/weighted"
    for ((k = 1; k <= n; k++)); do echo "${load[k]}" > "$work/load.$k"; done
    while read -r item; do printf '%s\t%s\n' "${owner[$item]:-0}" "$item"; done < "$items"
}

if [ "$MODE" = list ]; then
    enumerate "$work/attrs" || exit 1
    cat "$work/attrs"
    exit 0
fi

if [ "$MODE" = shards ]; then
    enumerate "$work/attrs" || exit 1
    assign "$work/attrs" "$SHARDS" > "$work/assign"
    for ((k = 1; k <= SHARDS; k++)); do
        c=0
        while IFS=$'\t' read -r o item; do [ "$o" = "$k" ] && c=$((c + 1)); done < "$work/assign"
        echo "shard $k/$SHARDS: $c items, estimated $(( $(cat "$work/load.$k") / MODEL_JOBS ))s"
        while IFS=$'\t' read -r o item; do [ "$o" = "$k" ] && echo "  $item"; done < "$work/assign"
    done
    exit 0
fi

if [ "$MODE" = merge ]; then
    : "${RIME_EVAL_OUT:?eval-each --merge: set RIME_EVAL_OUT, the merged file it writes}"
    enumerate "$work/attrs" || exit 1
    bad=0
    refuse() { echo "eval-each --merge: $*" >&2; bad=1; }
    declare -A seen_k=() count=() owner=() status=() val=() outp=() known=()
    n='' parts=0
    while read -r item; do known["$item"]=1; done < "$work/attrs"
    for d in "$PARTS"/*/; do
        [ -f "$d/shard" ] || continue
        parts=$((parts + 1))
        read -r k pn < "$d/shard"
        case "$k/$pn" in
            [1-9]*/[1-9]*) ;;
            *) refuse "part $d has no shard number ('$k/$pn')"; continue ;;
        esac
        case "$k$pn" in *[!0-9]*) refuse "part $d has no shard number ('$k/$pn')"; continue ;; esac
        [ "$k" -le "$pn" ] || refuse "part $d says shard $k of $pn"
        [ -n "$n" ] || n=$pn
        [ "$pn" = "$n" ] || refuse "part $d says shard $k of $pn, another says of $n"
        [ -z "${seen_k[$k]:-}" ] || refuse "shard $k of $n has two parts ($d and ${seen_k[$k]})"
        seen_k[$k]=$d
        if [ "$(cat "$d/all" 2> /dev/null)" != "$(cat "$work/attrs")" ]; then
            refuse "shard $k enumerated other items than this run does (different revision or nix args?)"
        fi
        while read -r item; do
            count["$item"]=$(( ${count[$item]:-0} + 1 ))
            owner["$item"]="${owner[$item]:+${owner[$item]},}$k"
            [ -n "${known[$item]:-}" ] || refuse "shard $k evaluated '$item', which is not a flake output this run lists"
        done < "$d/assigned"
        while read -r st item; do status["$k $item"]=$st; done < "$d/status"
        while IFS=$'\t' read -r item v o; do val["$k $item"]=$v; outp["$k $item"]=$o; done < "$d/drvs.tsv"
    done
    [ "$parts" -gt 0 ] || { echo "eval-each --merge: no shard parts in $PARTS" >&2; exit 1; }
    [ "$parts" = "$n" ] || refuse "$parts parts for $n shards"
    for ((k = 1; k <= ${n:-0}; k++)); do
        [ -n "${seen_k[$k]:-}" ] || refuse "shard $k of $n left no part (did its job run?)"
    done
    : > "$RIME_EVAL_OUT"
    while read -r item; do
        case "${count[$item]:-0}" in
            0) refuse "'$item' was assigned to no shard: nothing evaluated it"; continue ;;
            1) ;;
            *) refuse "'$item' was assigned to ${count[$item]} shards (${owner[$item]})"; continue ;;
        esac
        k=${owner[$item]}
        v=${val["$k $item"]:-} o=${outp["$k $item"]:-}
        if [ "${status["$k $item"]:-}" != OK ]; then
            refuse "'$item' did not evaluate in shard $k (${status["$k $item"]:-no status})"; continue
        fi
        case "$item" in
            *.drvPath)
                case "$v" in /nix/store/*.drv) ;; *) refuse "'$item' (shard $k) has no drvPath: '$v'"; continue ;; esac
                case "$o" in /nix/store/?*) ;; *) refuse "'$item' (shard $k) has no output path: '$o'"; continue ;; esac
                printf '%s\t%s\t%s\n' "$item" "$v" "$o" >> "$RIME_EVAL_OUT" ;;
            *)
                [ "$v" = true ] || { refuse "'$item' (shard $k) did not check out: '$v'"; continue; }
                printf '%s\t%s\n' "$item" "$v" >> "$RIME_EVAL_OUT" ;;
        esac
    done < "$work/attrs"
    if [ "$bad" != 0 ]; then
        echo "eval-each --merge: REFUSED: the shards did not evaluate every flake output exactly once, successfully" >&2
        exit 1
    fi
    echo "eval-each: merged $n shards: $(wc -l < "$work/attrs") items, each evaluated exactly once, all OK"
    exit 0
fi

enumerate "$work/attrs" || exit 1
assign "$work/attrs" "$SHARDS" > "$work/assign"
# Every item in exactly one shard 1..N, checked here before any evaluation (--merge checks it again, across parts).
while IFS=$'\t' read -r o item; do
    if [ "$o" -lt 1 ] || [ "$o" -gt "$SHARDS" ]; then
        echo "eval-each: '$item' was assigned to no shard (partition bug)" >&2; exit 2
    fi
done < "$work/assign"
: > "$work/mine"
while IFS=$'\t' read -r o item; do [ "$o" = "$SHARD" ] && echo "$item" >> "$work/mine"; done < "$work/assign"

# The kB value of one /proc key ("VmHWM:", "MemAvailable:"). Plain bash: the rime-nix container has no awk or sed.
field() {
    local k v _
    while read -r k v _; do [ "$k" = "$1" ] && { echo "$v"; return; }; done < "$2" 2>/dev/null
}
export -f field

one() {
    local attr="$1" t0 rc rss f="${1//\//_}" first
    # Light attributes run $JOBS at a time: start one only while GATE_MIB is free, so a few ~3 GB evaluators
    # finishing late cannot stack up on a 16 GB runner. The heavy pass runs one at a time with GATE_MIB=0.
    while [ "${GATE_MIB:-0}" -gt 0 ] && [ "$(( $(field MemAvailable: /proc/meminfo) / 1024 ))" -lt "$GATE_MIB" ]; do
        sleep 2
    done
    t0=$(date +%s)
    # GNU time is not on every runner: poll the evaluator's own peak RSS (VmHWM) while it runs.
    case "$attr" in
        nixosModules | overlays)
            nix eval "${NIX_ARGS[@]}" "$FLAKE#$attr" --apply \
                'm: builtins.all (v: builtins.isFunction v || builtins.isAttrs v || builtins.isPath v) (builtins.attrValues m)' \
                >"$work/val/$f" 2>"$work/$f.err" & ;;
        *) nix eval --raw "${NIX_ARGS[@]}" "$FLAKE#$attr" >"$work/val/$f" 2>"$work/$f.err" & ;;
    esac
    local pid=$! peak=0 cur
    while kill -0 "$pid" 2>/dev/null; do
        cur=$(field VmHWM: "/proc/$pid/status")
        [ "${cur:-0}" -gt "$peak" ] && peak=$cur
        sleep 0.5
    done
    wait "$pid"; rc=$?
    rss=$((peak / 1024))
    if [ "$rc" = 0 ]; then
        case "$attr" in
            nixosModules | overlays)
                [ "$(cat "$work/val/$f")" = true ] || { echo "$attr: not an attrset of functions/attrsets/paths" >> "$work/$f.err"; rc=1; } ;;
            *.drvPath)
                # The output path, where the .drv exists: the plan may run on a runner that never instantiated it.
                if first=$(nix-store -q --outputs "$(cat "$work/val/$f")" 2>>"$work/$f.err"); then
                    printf '%s\n' "${first%%$'\n'*}" > "$work/out/$f"
                else
                    rc=1
                fi ;;
        esac
    fi
    if [ "$rc" = 0 ]; then
        echo OK > "$work/st/$f"
        echo "OK $attr $(( $(date +%s) - t0 ))s ${rss}MiB"
        if [ "$rss" -gt "$HEAVY_MIB" ] && ! grep -qxF "${attr%.drvPath}" "$work/heavy" 2>/dev/null; then
            echo "::warning::eval-each: ${attr%.drvPath} peaked at ${rss} MiB; add it to nix/ci/eval-heavy.txt so it is evaluated alone"
        fi
    else
        echo FAIL > "$work/st/$f"
        echo "FAIL $attr $(( $(date +%s) - t0 ))s ${rss}MiB"
        tail -n 40 "$work/$f.err" | while IFS= read -r l; do printf '  %s\n' "$l"; done
        echo 1 > "$work/failed"
    fi
}
export -f one
export FLAKE work HEAVY_MIB
export NIX_ARGS_STR="${NIX_ARGS[*]-}"

: > "$work/light"; : > "$work/alone"
while read -r attr; do
    if grep -qxF "${attr%.drvPath}" "$work/heavy"; then echo "$attr" >> "$work/alone"; else echo "$attr" >> "$work/light"; fi
done < "$work/mine"

t_start=$(date +%s)
# xargs runs `one` in a fresh bash: hand it the extra args as a plain string (they never contain spaces of their own).
# shellcheck disable=SC2016 # expanded by the inner bash, on purpose
GATE_MIB="$GATE" xargs -P "$JOBS" -I{} bash -c 'NIX_ARGS=($NIX_ARGS_STR); one "$@"' _ {} < "$work/light"
# shellcheck disable=SC2016 # expanded by the inner bash, on purpose
GATE_MIB=0 xargs -P 1 -I{} bash -c 'NIX_ARGS=($NIX_ARGS_STR); one "$@"' _ {} < "$work/alone"

# This shard's results, in item order: drvs (RIME_EVAL_OUT and the part's drvs.tsv) and the part.
: > "$work/drvs.tsv"; : > "$work/status"
while read -r attr; do
    f="${attr//\//_}"
    st=$(cat "$work/st/$f" 2>/dev/null || echo FAIL)
    echo "$st $attr" >> "$work/status"
    [ "$st" = OK ] && [ -s "$work/val/$f" ] || continue
    if [ -s "$work/out/$f" ]; then
        printf '%s\t%s\t%s\n' "$attr" "$(cat "$work/val/$f")" "$(cat "$work/out/$f")" >> "$work/drvs.tsv"
    else
        printf '%s\t%s\n' "$attr" "$(cat "$work/val/$f")" >> "$work/drvs.tsv"
    fi
done < "$work/mine"
[ -z "${RIME_EVAL_OUT:-}" ] || cp "$work/drvs.tsv" "$RIME_EVAL_OUT"
if [ -n "${RIME_EVAL_PART:-}" ]; then
    mkdir -p "$RIME_EVAL_PART"
    echo "$SHARD $SHARDS" > "$RIME_EVAL_PART/shard"
    cp "$work/attrs" "$RIME_EVAL_PART/all"
    cp "$work/mine" "$RIME_EVAL_PART/assigned"
    cp "$work/status" "$RIME_EVAL_PART/status"
    cp "$work/drvs.tsv" "$RIME_EVAL_PART/drvs.tsv"
fi
echo "eval-each: shard $SHARD/$SHARDS: $(wc -l < "$work/mine") of $(wc -l < "$work/attrs") attributes evaluated, $JOBS at a time;" \
    "$(wc -l < "$work/alone") heavy ones alone; $(( $(date +%s) - t_start ))s (estimated $(( $(cat "$work/load.$SHARD") / MODEL_JOBS ))s)"
[ ! -e "$work/failed" ]
