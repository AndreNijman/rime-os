#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-firewall.sh — assertions against the shipped default-drop policy
#  and the helper that manages its exceptions.
#
#  ── Why this file exists ────────────────────────────────────────────────────
#  Rime shipped with no firewall at all: firewalld absent, nftables present with
#  an empty ruleset, every bound port reachable. The policy that fixes it is a
#  static file, which makes it exactly the kind of thing that rots silently — a
#  rule deleted in a refactor changes nothing anybody notices until it matters.
#
#  So the base policy is asserted line by line, and each assertion says what
#  breaks without it. Three of them are not about security at all: without
#  established/related, ICMP and IPv6 neighbour discovery, the machine reads as
#  "the network is broken" and the firewall is the last place anyone looks.
#
#  ── What it deliberately does NOT do ────────────────────────────────────────
#  It never loads the ruleset. `nft -c` parses without applying, and a suite
#  that installed a default-drop policy on the machine running it would be a
#  suite that disconnects a remote developer mid-run.
#
#  PASS = the policy parses, every load-bearing rule is present, and the helper
#         refuses what it should.
#
#  Run from anywhere: ./tests/test-rime-firewall.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 2

RULES=files/system/nftables/rime.nft
HELPER=files/system/libexec/rime-firewall
UNIT=files/system/units/rime-firewall.service
CATALOGUE=files/system/firewall/services
for f in "$RULES" "$HELPER" "$UNIT" "$CATALOGUE"; do
    [ -f "$f" ] || { echo "cannot find $f"; exit 2; }
done

WORK=$(mktemp -d /tmp/rime-fw-test.XXXXXX) || exit 2
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0; skip=0
ok()      { printf 'PASS  %-52s\n' "$1"; pass=$((pass+1)); }
bad()     { printf 'FAIL  %-52s %s\n' "$1" "$2"; fail=$((fail+1)); }
skipped() { printf 'SKIP  %-52s %s\n' "$1" "$2"; skip=$((skip+1)); }

has() {  # $1 = case name, $2 = extended regex the policy must contain
    if grep -qE "$2" "$RULES"; then ok "$1"; else bad "$1" "no line matching $2"; fi
}

echo "── the policy parses ──────────────────────────────────────────────────"
# `nft -c` applies nothing, but libnftables still opens a netlink socket and
# lists the live ruleset before it will look at a file, and that needs
# CAP_NET_ADMIN. This case used to SKIP for every non-root caller — which is to
# say it never once parsed the policy in CI, and the image build hit the same
# wall (`cache initialization failed: Operation not permitted`) and could not
# build at all. A private user+network namespace of our own hands nft an empty
# ruleset it is allowed to read, so an unprivileged run parses for real.
if ! command -v nft >/dev/null 2>&1; then
    skipped "the ruleset parses" "nft is not installed"
else
    nft_out="$(nft -c -f "$RULES" 2>&1)" && nft_rc=0 || nft_rc=$?
    if [ "$nft_rc" != 0 ] && command -v unshare >/dev/null 2>&1; then
        ns_out="$(unshare --user --map-root-user --net \
                    nft -c -f "$RULES" 2>&1)" && nft_rc=0 || nft_rc=$?
        # unshare can be refused in its own right. Keep nft's own words in that
        # case: reporting "unshare failed" as a FAIL on "the ruleset parses"
        # would be a could-not-run wearing a syntax error's clothes.
        case "$ns_out" in unshare:*) : ;; *) nft_out="$ns_out" ;; esac
    fi
    if [ "$nft_rc" = 0 ]; then
        ok "the ruleset parses"
    elif printf '%s' "$nft_out" | pipe_has 'cache initialization failed'; then
        # Could-not-run, said as could-not-run. Not a pass and not a syntax error.
        skipped "the ruleset parses" "nft cannot reach netlink here, even in a private netns"
    else
        bad "the ruleset parses" "$(printf '%s' "$nft_out" | head -1)"
    fi
fi

echo
echo "── default drop, which is the entire point ────────────────────────────"
has "input drops by default"        '^\s*type filter hook input priority filter; policy drop;$'
has "output is allowed"             '^\s*type filter hook output priority filter; policy accept;$'

# There WAS a forward chain, dropping by default, with a comment saying podman
# was unaffected because it "hooks at its own priority in its own table".
# tests/test-rime-firewall-live.sh measured that: every base chain at a hook is
# evaluated, and a drop in any one of them is final, so this chain overrode
# netavark, libvirt and incus and killed every rootful container's networking.
# ip_forward is 0 until something deliberately turns it on, and the thing that
# turns it on brings its own policy.
# Not anchored to the line start: a forward chain written on one line is the
# same chain, and an anchored pattern let exactly that mutant through.
if grep -q 'type filter hook forward' "$RULES"; then
    bad "no forward chain, which would override every container's own table" \
        "a drop here is final regardless of what netavark accepts"
else
    ok "no forward chain, which would override every container's own table"
fi

# `nft -f` merges a table block into an existing table rather than replacing
# it. Without these two lines a second load appends every rule again — measured
# in a namespace, 14 accepts became 28 — and a doubled `limit rate` is silently
# twice the rate it says.
if grep -qx 'table inet rime' "$RULES" && grep -qx 'delete table inet rime' "$RULES"; then
    ok "a second load replaces the table instead of appending to it"
else
    bad "a second load replaces the table instead of appending to it" \
        "no 'table inet rime' / 'delete table inet rime' preamble"
fi

echo
echo "── the rules without which the machine looks broken, not protected ────"
has "replies to our own traffic pass" '^\s*ct state established,related accept$'
has "loopback passes"                 '^\s*iif lo accept$'
has "ICMP errors pass (path MTU)"     'destination-unreachable, time-exceeded'
has "IPv6 neighbour discovery passes" 'nd-neighbor-solicit'
has "DHCP replies pass"               '^\s*udp sport 67 udp dport 68 accept$'

echo
echo "── ssh, which is load-bearing rather than incidental ──────────────────"
# rime host run, rime build --on and remote agents are all ssh. A policy without
# it strands the user on the machine they were driving from.
has "ssh is open"                     '^\s*tcp dport 22 accept$'

echo
echo "── exceptions cannot damage the base policy ───────────────────────────"
has "exceptions live in a tcp set"    'tcp dport @allowed_tcp accept'
has "exceptions live in a udp set"    'udp dport @allowed_udp accept'
if grep -qE '^\s*(add|insert) rule' "$HELPER"; then
    bad "the helper never writes a rule" "it edits the chain instead of the sets"
else
    ok "the helper never writes a rule"
fi

echo
echo "── echo responders are rate limited ───────────────────────────────────"
has "ICMP echo is rate limited"       'icmp type \{ echo-request \} limit rate'
has "ICMPv6 echo is rate limited"     'icmpv6 type \{ echo-request \} limit rate'

echo
echo "── the unit ───────────────────────────────────────────────────────────"
grep -q '^Before=network-pre.target' "$UNIT" \
    && ok "loads before the network is configured" \
    || bad "loads before the network is configured" "there would be an unfiltered window every boot"
grep -q '^Conflicts=nftables.service' "$UNIT" \
    && ok "cannot fight nftables.service for the ruleset" \
    || bad "cannot fight nftables.service for the ruleset" "two owners, last one wins silently"
grep -q '^WantedBy=multi-user.target' "$UNIT" \
    && ok "is enabled by an install section" \
    || bad "is enabled by an install section" "a firewall that ships disabled is documentation"

echo
echo "── the helper ─────────────────────────────────────────────────────────"
bash -n "$HELPER" && ok "the helper parses" || bad "the helper parses" "syntax error"

out=$(bash "$HELPER" list 2>&1)
if grep -q '^ssh ' <<<"$out" && grep -qi 'NAME .*PROTO .*PORT' <<<"$out"; then
    ok "list names services rather than ports"
else
    bad "list names services rather than ports" "$(head -1 <<<"$out")"
fi

# Every catalogue line must be name/proto/port/description, or `allow` writes a
# malformed exception file and the reload silently drops it.
badline=$(grep -vE '^\s*(#|$)' "$CATALOGUE" | grep -vE '^[a-z0-9-]+ +(tcp|udp) +[0-9]+ +\S' | head -1)
[ -z "$badline" ] && ok "every catalogue entry is well formed" \
                  || bad "every catalogue entry is well formed" "$badline"

# The helper carries a copy of the catalogue so it still works when the image's
# is missing, and nothing kept the two the same. A drifted fallback is worse
# than no fallback: `allow` succeeds, writes a file, and opens a port the user
# did not ask for — or refuses a service the catalogue lists.
shipped=$(grep -vE '^\s*(#|$)' "$CATALOGUE" | awk '{$1=$1};1')
# The FIRST heredoc only: usage() has one too, and swallowing it makes this
# case fail for a reason that has nothing to do with the catalogue.
builtin=$(awk '/cat <<.EOF.$/{if(!seen){f=1;seen=1;next}} /^EOF$/{f=0} f' "$HELPER" | awk 'NF{$1=$1};1')
if [ -z "$builtin" ]; then
    bad "the helper's built-in catalogue matches the shipped one" "no built-in catalogue found in the helper"
elif [ "$shipped" = "$builtin" ]; then
    ok "the helper's built-in catalogue matches the shipped one"
else
    bad "the helper's built-in catalogue matches the shipped one" \
        "$(diff <(echo "$shipped") <(echo "$builtin") | head -3 | tr '\n' ' ')"
fi

if [ "$(id -u)" = 0 ]; then
    skipped "allow refuses an unprivileged caller"  "running as root"
    skipped "reload refuses an unprivileged caller" "running as root"
else
    # allow and reload both change state and must refuse. `deny` is checked
    # separately below, because it answers "that was never open" first and only
    # reaches the root gate for a service that actually is.
    for verb in allow reload; do
        out=$(bash "$HELPER" "$verb" ssh 2>&1); rc=$?
        if [ "$rc" != 0 ] && grep -qi 'needs root' <<<"$out"; then
            ok "$verb refuses an unprivileged caller"
        else
            bad "$verb refuses an unprivileged caller" "rc=$rc: $(head -1 <<<"$out")"
        fi
    done
fi

# Closing something that was never open is a mistake worth naming, and naming it
# costs no privilege. Asking for a password first and *then* saying "that was
# never open" is the command this avoids being.
out=$(bash "$HELPER" deny syncthing 2>&1); rc=$?
if [ "$rc" != 0 ] && grep -qi 'was not allowed' <<<"$out"; then
    ok "deny says a service was never open before asking for a password"
else
    bad "deny says a service was never open before asking for a password" "rc=$rc: $(head -1 <<<"$out")"
fi

out=$(bash "$HELPER" allow definitely-not-a-service 2>&1)
grep -qi 'no service called' <<<"$out" && ok "an unknown service is named, not opened" \
    || bad "an unknown service is named, not opened" "$(head -1 <<<"$out")"

# The name becomes a path. `deny` reaches the path before the catalogue, so
# without a check on the name, `deny ../../../etc/issue` was an `rm -f` outside
# the exception directory, run as root. "was not allowed" is the WRONG answer
# here and is asserted against: it would mean the tool had already gone looking
# where it should not.
for evil in ../../../etc/issue /etc/issue 'a;b' 'a b'; do
    out=$(bash "$HELPER" deny "$evil" 2>&1); rc=$?
    if [ "$rc" != 0 ] && grep -qi 'not a service name' <<<"$out"; then
        ok "deny refuses '$evil' as a name rather than as a path"
    else
        bad "deny refuses '$evil' as a name rather than as a path" "rc=$rc: $(head -1 <<<"$out")"
    fi
done

# The lesson this codebase learned the hard way today, in a fourteenth place:
# `nft list` needs CAP_NET_ADMIN, and answering "no rules" to a caller who
# merely lacks permission is the worst answer a firewall tool can give.
if grep -q 'permission denied|operation not permitted' <<<"$(grep -o 'permission denied|operation not permitted' "$HELPER")"; then
    ok "status tells you it cannot look, rather than reporting nothing"
else
    bad "status tells you it cannot look, rather than reporting nothing" \
        "ruleset_state does not distinguish EACCES from an empty ruleset"
fi


echo
echo "── status --json, the surface a program reads ──────────────────────────"
# Rime Shell's Firewall settings page reads this tool, and until now the only
# thing there was to read was its PROSE. That coupling failed the first time
# the prose moved, and it failed the way this kind of coupling always fails —
# silently, with every suite on both sides staying green:
#
#   c7a28f2c put the shared-links line into the status screen, between the
#   "always allowed" heading and the "exceptions you have added" heading. The
#   shell assigns every unprefixed line under the first heading to its
#   "always allowed" field, so on a machine where the ruleset is readable the
#   settings page started reporting "not sharing this machine's connection on
#   any link" as the list of traffic the firewall never drops. The shell's
#   fixture had been captured before that line existed, so its suite went on
#   checking a shape this tool can no longer print.
#
# So the cases below are about a SHAPE, and two of them are specifically about
# the prose and the JSON not being allowed to drift apart again.
#
# ── WHY A COPY OF THE HELPER, AND WHAT THAT WOULD COST IF IT WENT WRONG ─────
# CONF_DIR is a readonly constant naming the live machine's
# /etc/rime/firewall.d. A suite must never write exception files there — this
# one runs on Andre's laptop — so the fixtures go in a temporary directory and
# the cases drive a COPY of the helper whose one constant is redirected.
#
# A copy is a weaker gate than the original, and it fails in a specific way:
# if the substitution matches nothing the copy is identical to its source, and
# every verdict below silently becomes a verdict about whatever is in the live
# machine's exception directory. So the copy is diffed against its source and a
# substitution that did not apply is a hard failure, not a skip.
JWORK="$WORK/json"; mkdir -p "$JWORK/bin" "$JWORK/conf"

if ! command -v python3 >/dev/null 2>&1; then
    skipped "status --json is valid JSON" "python3 is not installed"
else

# $1 = destination, $2... = extra sed programs (the mutants use these).
json_helper() {
    local dest="$1"; shift
    sed "s#^readonly CONF_DIR=.*#readonly CONF_DIR=$JWORK/conf#" "$HELPER" > "$dest" || return 1
    local prog
    for prog in "$@"; do sed -i "$prog" "$dest" || return 1; done
    # Not `cmp` against the source — that only proves SOMETHING changed, and
    # the CONF_DIR line always changes. This asks the question that matters.
    grep -q "^readonly CONF_DIR=$JWORK/conf\$" "$dest"
}

# $1 = what `nft list table` does, $2 = the `elements = {...}` body or empty.
# Two verdicts, because "the ruleset is not loaded" and "you are not allowed to
# look at the ruleset" are different answers and this tool exists to keep them
# apart.
jnft() {
    case "$1" in
        loaded)     printf '#!/bin/bash\ncase "$*" in\n  "list table inet rime") echo ok; exit 0;;\n  "list set inet rime hotspot_ifaces") printf %%s "%s"; exit 0;;\nesac\nexit 0\n' "$2" > "$JWORK/bin/nft" ;;
        notloaded)  printf '#!/bin/bash\necho "Error: No such file or directory" >&2\nexit 1\n' > "$JWORK/bin/nft" ;;
        unreadable) printf '#!/bin/bash\necho "Error: Operation not permitted" >&2\nexit 1\n' > "$JWORK/bin/nft" ;;
        absent)     rm -f "$JWORK/bin/nft"; return 0 ;;
    esac
    chmod +x "$JWORK/bin/nft"
}

# `absent` has to mean nft is on NO path, so these run with a PATH holding only
# the shim directory and the handful of tools the helper actually calls. An
# earlier version of this left /usr/bin on the PATH, found the real nft, and
# reported `unreadable` — a test asserting the wrong state and passing.
MINBIN="$JWORK/min"; mkdir -p "$MINBIN"
for c in bash basename sed tr grep paste cat id; do
    src="$(command -v "$c" 2>/dev/null)" && ln -sf "$src" "$MINBIN/$c"
done

jrun() {  # $1 = helper, $2 = PATH prefix mode (shim|min), rest = args
    local fw="$1" mode="$2"; shift 2
    if [ "$mode" = min ]; then
        PATH="$MINBIN" bash "$fw" "$@" 2>/dev/null
    else
        PATH="$JWORK/bin:$PATH" bash "$fw" "$@" 2>/dev/null
    fi
}

# Reads one field out of a document, or prints __BAD__ if it is not JSON at
# all. `json.load` and not a grep: "is this parseable" is half of what these
# cases are asking, and a grep would answer yes to a broken document.
jfield() {  # stdin = document, $1 = python expression over `d`
    python3 -c '
import json,sys
try:    d = json.load(sys.stdin)
except Exception: print("__BAD__"); raise SystemExit(0)
try:    print(eval(sys.argv[1]))
except Exception as e: print("__MISSING__")
' "$1" 2>/dev/null
}

# Every --json verdict, in a function, so a mutated helper can be driven
# through exactly the same questions the real one is.
#   $1 = helper path, $2 = 1 to stay quiet (mutant runs)
# Sets jpass / jfail.
json_cases() {
    local fw="$1" quiet="${2:-0}" got want
    jpass=0; jfail=0
    jok()  { jpass=$((jpass+1)); [ "$quiet" = 1 ] || ok "$1"; }
    jbad() { jfail=$((jfail+1)); [ "$quiet" = 1 ] || bad "$1" "$2"; }

    # ── the four policy states ──────────────────────────────────────────────
    # The word is ruleset_state's enum verbatim. A caller that has to map
    # sentences back onto these four is the problem this surface removes.
    rm -f "$JWORK/conf"/*.conf
    for state in loaded notloaded unreadable absent; do
        jnft "$state" ""
        if [ "$state" = absent ]; then got="$(jrun "$fw" min status --json | jfield 'd["policy"]')"
        else                           got="$(jrun "$fw" shim status --json | jfield 'd["policy"]')"; fi
        [ "$got" = "$state" ] \
            && jok "policy is \"$state\" when the ruleset is $state" \
            || jbad "policy is \"$state\" when the ruleset is $state" "got [$got]"
    done

    # ── hotspot_links: null is not [] ───────────────────────────────────────
    # [] says "this machine is sharing nothing". A caller that could not read
    # the ruleset has not learned that, and a settings page rendering [] as
    # "not sharing" would be stating a fact nobody checked.
    for state in notloaded unreadable; do
        jnft "$state" ""
        got="$(jrun "$fw" shim status --json | jfield 'd["hotspot_links"]')"
        [ "$got" = "None" ] \
            && jok "hotspot_links is null, not [], when the ruleset is $state" \
            || jbad "hotspot_links is null, not [], when the ruleset is $state" "got [$got]"
    done

    jnft loaded '	set hotspot_ifaces {
		type ifname
	}'
    got="$(jrun "$fw" shim status --json | jfield 'd["hotspot_links"]')"
    [ "$got" = "[]" ] \
        && jok "hotspot_links is [] when the ruleset is loaded and nothing is shared" \
        || jbad "hotspot_links is [] when the ruleset is loaded and nothing is shared" "got [$got]"

    jnft loaded '	set hotspot_ifaces {
		elements = { "wlan0" }
	}'
    got="$(jrun "$fw" shim status --json | jfield '",".join(d["hotspot_links"])')"
    [ "$got" = "wlan0" ] \
        && jok "one shared link is named" \
        || jbad "one shared link is named" "got [$got]"

    # The case the prose parser got wrong for months: nft wraps a set's
    # elements one per line as soon as there are TWO, not at some width.
    jnft loaded '	set hotspot_ifaces {
		elements = { "wlan0",
			     "enp0s31f6" }
	}'
    got="$(jrun "$fw" shim status --json | jfield '",".join(d["hotspot_links"])')"
    [ "$got" = "wlan0,enp0s31f6" ] \
        && jok "two shared links survive nft wrapping them one per line" \
        || jbad "two shared links survive nft wrapping them one per line" "got [$got]"

    # ── exceptions ──────────────────────────────────────────────────────────
    got="$(jrun "$fw" shim status --json | jfield 'd["exceptions"]')"
    [ "$got" = "[]" ] \
        && jok "an empty exception directory is [] and still parses" \
        || jbad "an empty exception directory is [] and still parses" "got [$got]"

    printf 'udp 5353\n' > "$JWORK/conf/mdns.conf"
    got="$(jrun "$fw" shim status --json | jfield '"%s/%s/%s/%s" % (d["exceptions"][0]["name"], d["exceptions"][0]["proto"], d["exceptions"][0]["port"], d["exceptions"][0]["rejected"])')"
    [ "$got" = "mdns/udp/5353/False" ] \
        && jok "a working exception carries its name, proto and port" \
        || jbad "a working exception carries its name, proto and port" "got [$got]"

    # THE one that matters. A rejected exception must not carry a port, because
    # a caller rendering the row the same way it renders a working one would be
    # telling the user a port is open when the reload refused it.
    printf 'tcp notaport\n' > "$JWORK/conf/broken.conf"
    got="$(jrun "$fw" shim status --json | jfield '";".join("%s|%s|%s|%s" % (e["name"], e["rejected"], e["proto"], e["port"]) for e in d["exceptions"])')"
    [ "$got" = "broken|True||;mdns|False|udp|5353" ] \
        && jok "a rejected exception is flagged and carries NO port" \
        || jbad "a rejected exception is flagged and carries NO port" "got [$got]"

    got="$(jrun "$fw" shim status --json | jfield 'd["exceptions"][0]["detail"]')"
    [ "$got" = "tcp notaport" ] \
        && jok "and it says what was rejected" \
        || jbad "and it says what was rejected" "got [$got]"

    # ── the escaping trap ───────────────────────────────────────────────────
    # `name` comes from a filename and `detail` from a line the user typed.
    # Neither has been through valid_name: this tool reports what is in the
    # directory, not only what it put there. An unescaped quote does not
    # produce a wrong field, it produces a document nothing can parse — a
    # settings page reading "could not be read" because somebody named a file
    # my"printer.conf.
    rm -f "$JWORK/conf"/*.conf
    printf 'tcp no"t\\a\n' > "$JWORK/conf/my\"weird\\name.conf"
    got="$(jrun "$fw" shim status --json | jfield '"%s => %s" % (d["exceptions"][0]["name"], d["exceptions"][0]["detail"])')"
    want='my"weird\name => tcp no"t\a'
    [ "$got" = "$want" ] \
        && jok "a quote and a backslash in a name and a detail survive as data" \
        || jbad "a quote and a backslash in a name and a detail survive as data" "got [$got]"

    # ── the prose and the JSON are not allowed to drift ─────────────────────
    # This is the pair of cases the whole section exists for. Both render from
    # one reader; if somebody ever gives them two, these go red.
    rm -f "$JWORK/conf"/*.conf
    printf 'udp 5353\n'     > "$JWORK/conf/mdns.conf"
    printf 'tcp notaport\n' > "$JWORK/conf/broken.conf"
    printf 'tcp 22000\n'    > "$JWORK/conf/syncthing.conf"
    jnft unreadable ""
    local prose_names json_names
    prose_names="$(jrun "$fw" shim status |
        sed -n '/^exceptions you have added:/,$p' | tail -n +2 |
        awk 'NF {print $1}' | sort | paste -sd, -)"
    json_names="$(jrun "$fw" shim status --json | jfield '",".join(sorted(e["name"] for e in d["exceptions"]))')"
    [ -n "$json_names" ] && [ "$prose_names" = "$json_names" ] \
        && jok "the prose and the JSON name the same exceptions" \
        || jbad "the prose and the JSON name the same exceptions" \
                "prose [$prose_names] json [$json_names]"

    local prose_always json_always
    prose_always="$(jrun "$fw" shim status |
        sed -n '/^always allowed, and not removable here:/{n;s/^  //;p;q}')"
    json_always="$(jrun "$fw" shim status --json | jfield 'd["always_allowed"]')"
    [ -n "$json_always" ] && [ "$prose_always" = "$json_always" ] \
        && jok "the prose and the JSON say the same thing is always allowed" \
        || jbad "the prose and the JSON say the same thing is always allowed" \
                "prose [$prose_always] json [$json_always]"

    # ── the argument itself ─────────────────────────────────────────────────
    got="$(jrun "$fw" shim status | head -1)"
    case "$got" in
        rime-firewall:*) jok "status with no argument is still prose" ;;
        *)               jbad "status with no argument is still prose" "got [$got]" ;;
    esac

    # A flag this tool does not have must be refused rather than quietly
    # answered with the default: a caller asking for --yaml and getting prose
    # would parse it as YAML.
    out="$(PATH="$JWORK/bin:$PATH" bash "$fw" status --yaml 2>&1)"; rc=$?
    if [ "$rc" != 0 ] && grep -qi 'usage: rime firewall status' <<<"$out"; then
        jok "an argument that is not --json is refused, not ignored"
    else
        jbad "an argument that is not --json is refused, not ignored" "rc=$rc: $(head -1 <<<"$out")"
    fi
}

if ! json_helper "$JWORK/fw"; then
    bad "the suite's redirected copy of the helper was built" \
        "the CONF_DIR substitution did not apply, so every --json case below would have been a verdict about the LIVE machine's /etc/rime/firewall.d"
else
    json_cases "$JWORK/fw" 0
    pass=$((pass + jpass)); fail=$((fail + jfail))

    # ── the mutants ─────────────────────────────────────────────────────────
    # Each breaks one property and must turn the block above red. A suite that
    # cannot be made to fail is not measuring anything, and this codebase has
    # now caught three assertions that could not fail — so these are not
    # optional decoration.
    #
    # Each mutant is checked for having APPLIED before its verdict is believed:
    # a sed that matched nothing leaves a helper identical to the real one, and
    # "the mutant did not redden the suite" would then be a true statement
    # about the wrong file.
    caught=0; missed=0
    mutate() {  # $1 = description, $2 = sed program
        if ! json_helper "$JWORK/mut" "$2"; then
            bad "mutant applies: $1" "json_helper failed"; missed=$((missed+1)); return
        fi
        if cmp -s "$JWORK/mut" "$JWORK/fw"; then
            bad "mutant applies: $1" "the sed matched nothing — this mutant proves nothing"
            missed=$((missed+1)); return
        fi
        json_cases "$JWORK/mut" 1
        if [ "$jfail" -gt 0 ]; then
            ok "mutant caught: $1"; caught=$((caught+1))
        else
            bad "mutant caught: $1" "$jpass verdicts all passed against a helper that is wrong"
            missed=$((missed+1))
        fi
    }

    # M1 — the prose stops rendering from ALWAYS_ALLOWED. The drift this whole
    #      surface exists because of, reintroduced deliberately.
    mutate "the prose hardcodes its own always-allowed sentence" \
           's#^    echo "  \$ALWAYS_ALLOWED"#    echo "  established replies and nothing else"#'
    # M2 — json_str stops escaping backslashes.
    mutate "json_str stops escaping backslashes" \
           's#^    s="\${s//\\\\/.*#    :#'
    # M3 — hotspot_links becomes [] when the ruleset could not be read.
    mutate "hotspot_links is [] rather than null when nobody could look" \
           's#^        printf .  "hotspot_links": null,.n.#        printf "  \\"hotspot_links\\": [],\\n"#'
    # M4 — a rejected exception is reported with the port that was refused.
    mutate "a rejected exception carries the port the reload refused" \
           's#"proto": "", "port": "", "rejected": true#"proto": "tcp", "port": "9999", "rejected": true#'
    # M5 — the JSON gets its own loop over the directory and skips a file.
    mutate "the JSON exception list drops an entry the prose keeps" \
           's#^    each_exception _json_row#    each_exception _json_row | head -c 200#'
    # M6 — --json falls through to the prose.
    mutate "status --json answers with prose" \
           's#^        --json) cmd_status_json; return ;;#        --json) ;;#'
    # M7 — a set with two elements loses the first, which is how nft's wrapping
    #      broke the old parser.
    mutate "the shared-link reader eats the first name" \
           's#^    grep -v ..\$.$#    grep -v "^\$" | tail -n +2#'

    if [ "$missed" -eq 0 ]; then
        ok "every --json mutant reddened the block above ($caught of $caught)"
    else
        bad "every --json mutant reddened the block above" "$missed of $((caught+missed)) did not"
    fi
fi
fi
echo
echo "── the NetworkManager dispatcher, which decides when a link opens ──────"
# Everything above reads a file. This runs the dispatcher, because its security
# property is not visible in the policy: `up` must open a link ONLY after nmcli
# confirms the connection is `shared`, and `down` must close one WITHOUT asking
# anything. Getting the first wrong opens DHCP and a resolver on every network
# this machine joins; getting the second wrong leaves them open after sharing
# stops, on a link the user thinks is a normal Wi-Fi connection.
#
# No firewall runs here. RIME_HOTSPOT_FW points the script at a shim that logs
# what it was asked to do, so each case is a verdict about the call that was
# made rather than about a rule.
DISPATCH=files/system/NetworkManager/dispatcher.d/50-rime-hotspot-firewall
if [ ! -f "$DISPATCH" ]; then
    bad "the hotspot dispatcher is present" "$DISPATCH is missing, so nothing fills the set"
else
    bash -n "$DISPATCH" && ok "the hotspot dispatcher parses" \
                        || bad "the hotspot dispatcher parses" "syntax error"

    DWORK="$WORK/dispatch"; mkdir -p "$DWORK/bin"
    printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s/fw.log"\n' "$DWORK" > "$DWORK/fw"
    chmod +x "$DWORK/fw"
    # Logs the query as well as answering it, so "down never asked" can be
    # asserted rather than assumed.
    printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s/nmcli.log"\nif [ -e "%s/hang" ]; then sleep 60; fi\ncat "%s/method" 2>/dev/null\n' \
        "$DWORK" "$DWORK" "$DWORK" > "$DWORK/bin/nmcli"
    chmod +x "$DWORK/bin/nmcli"

    dispatch() {  # $1 = iface, $2 = action, rest = env assignments
        local iface=$1 action=$2; shift 2
        rm -f "$DWORK/fw.log" "$DWORK/nmcli.log"
        env PATH="$DWORK/bin:$PATH" RIME_HOTSPOT_FW="$DWORK/fw" \
            CONNECTION_UUID=11111111-2222-3333-4444-555555555555 \
            "$@" bash "$DISPATCH" "$iface" "$action" >/dev/null 2>&1
    }
    fwlog()    { cat "$DWORK/fw.log" 2>/dev/null; }
    nmclilog() { cat "$DWORK/nmcli.log" 2>/dev/null; }

    echo shared > "$DWORK/method"
    dispatch wlan0 up
    [ "$(fwlog)" = "hotspot add wlan0" ] \
        && ok "up on a shared connection opens that link" \
        || bad "up on a shared connection opens that link" "called: [$(fwlog)]"

    # The one that matters. Every normal Wi-Fi connection coming up runs this.
    echo auto > "$DWORK/method"
    dispatch wlan0 up
    [ -z "$(fwlog)" ] \
        && ok "up on an ordinary connection opens nothing" \
        || bad "up on an ordinary connection opens nothing" \
               "a café Wi-Fi would open DHCP and a resolver: [$(fwlog)]"

    # nmcli answering nothing is not nmcli answering `shared`.
    : > "$DWORK/method"
    dispatch wlan0 up
    [ -z "$(fwlog)" ] \
        && ok "up opens nothing when nmcli answers nothing" \
        || bad "up opens nothing when nmcli answers nothing" "[$(fwlog)]"

    # A dispatcher script that blocks holds a slot for every later one on
    # NetworkManager's queue, so the query is capped — and a cap that expires
    # must not be read as `shared`.
    echo shared > "$DWORK/method"; : > "$DWORK/hang"
    start=$SECONDS
    dispatch wlan0 up
    elapsed=$(( SECONDS - start ))
    rm -f "$DWORK/hang"
    if [ -z "$(fwlog)" ] && [ "$elapsed" -lt 30 ]; then
        ok "an nmcli that hangs neither opens the link nor blocks NM"
    else
        bad "an nmcli that hangs neither opens the link nor blocks NM" \
            "${elapsed}s, called: [$(fwlog)]"
    fi

    # down closes without asking. The profile may already be gone by then, and
    # "I could not check" must never be the reason a link stays open.
    echo auto > "$DWORK/method"
    dispatch wlan0 down
    [ "$(fwlog)" = "hotspot remove wlan0" ] \
        && ok "down closes the link whatever nmcli would have said" \
        || bad "down closes the link whatever nmcli would have said" "called: [$(fwlog)]"
    [ -z "$(nmclilog)" ] \
        && ok "and down does not ask nmcli anything at all" \
        || bad "and down does not ask nmcli anything at all" \
               "a lookup that fails would skip the close: [$(nmclilog)]"

    # NM hands the device name in $1 and the addressed interface in the
    # environment; the rules have to name the second.
    echo shared > "$DWORK/method"
    dispatch wlan0 up DEVICE_IP_IFACE=ap0
    [ "$(fwlog)" = "hotspot add ap0" ] \
        && ok "the interface the rules name is the one NM addressed" \
        || bad "the interface the rules name is the one NM addressed" "called: [$(fwlog)]"

    # No interface, nothing to do, and nothing said to a firewall about it.
    dispatch "" up
    [ -z "$(fwlog)" ] \
        && ok "an empty interface name reaches no firewall command" \
        || bad "an empty interface name reaches no firewall command" "[$(fwlog)]"

    # An action the script does not handle must fall through silently rather
    # than through whichever branch happens to be last.
    dispatch wlan0 connectivity-change
    [ -z "$(fwlog)" ] \
        && ok "an unhandled NM action changes nothing" \
        || bad "an unhandled NM action changes nothing" "[$(fwlog)]"
fi

echo
# ── sing-box's tunnel ────────────────────────────────────────────────────────
# With sing-box's system stack every TCP connection through the VPN arrives as a
# NEW inbound connection on sb-tun; without this rule the drop policy killed all
# of them (the VPN "connected" and no TCP got through). Exactly sb-tun: a
# wildcard would also admit OpenVPN's tun0, whose far side is a remote network.
grep -qE '^[[:space:]]*iifname "sb-tun" accept$' "$RULES" \
    && ok "input accepts sing-box's own tunnel, sb-tun" \
    || bad "input accepts sing-box's own tunnel, sb-tun" "missing: TCP through the VPN is dropped"
grep -qE 'iifname "tun\*"|iifname "tun[0-9]*" accept' "$RULES" \
    && bad "no wildcard tunnel accept (would admit a remote VPN)" "found one" \
    || ok "no wildcard tunnel accept (would admit a remote VPN)"

printf 'rime-firewall: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
