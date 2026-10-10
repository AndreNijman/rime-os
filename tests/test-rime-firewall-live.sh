#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-firewall-live.sh — the default-drop policy, ENFORCING.
#
#  tests/test-rime-firewall.sh reads the policy. It says so at the top, and it
#  is right to: a suite that installed a default-drop policy on the machine
#  running it would disconnect a remote developer mid-run. But everything that
#  file can prove is that a string is present in a file, and the acceptance
#  criteria for this feature are all of the form "the machine still works" —
#  which no grep can answer. Until this file existed the policy had never been
#  loaded into a kernel at all.
#
#  ── How it is safe to run anywhere ──────────────────────────────────────────
#  Four network namespaces and three veth pairs. The policy is loaded inside
#  one of them and nowhere else; the host's own ruleset, routes and interfaces
#  are never touched, and every namespace is torn down on exit including on a
#  signal. A netns is a real netfilter path with real conntrack, real ICMP and
#  real neighbour discovery, so the assertions below are about kernel
#  behaviour rather than about text.
#
#      far ──1280── net ──1500── HOST ──1500── lan
#                    ↑            ↑             ↑
#            the network      the policy    something behind it
#
#  `far` sits behind a link too small for a 1400-byte packet, which is what
#  makes path-MTU discovery testable: `net` has to send an ICMP
#  fragmentation-needed back and HOST has to accept it. `lan` sits behind HOST,
#  which is what makes the forward chain testable.
#
#  ── What it deliberately does NOT prove ─────────────────────────────────────
#  That a real ssh session survives. A netns cannot answer that, because the
#  connection that matters is the one carrying this script. That assertion is
#  in tests/test-rime-firewall-ssh.sh, which runs on a machine you are willing
#  to lose, behind a dead-man's switch.
#
#  Nor does it prove boot ordering: nothing here boots.
#
#  ── Modes ───────────────────────────────────────────────────────────────────
#      (no argument)   run every case against the shipped policy and helper
#      --self-test     run the suite once per mutant and require that each
#                      mutant reddens the case it was written for. A safety net
#                      that has never been tested against its own case is not a
#                      safety net; this project has found three of those.
#      --mutate-rules SED / --mutate-helper SED
#                      run the suite against an edited copy. Used by
#                      --self-test, and by hand when adding a case.
#
#  Needs root, `ip`, `nft` and `socat`. Skips cleanly (status 0) without them,
#  because a skipped case that reports a pass is the failure mode this file is
#  most at risk of.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

SRC_RULES="${RIME_FW_RULES:-$ROOT/files/system/nftables/rime.nft}"
SRC_HELPER="${RIME_FW_HELPER:-$ROOT/files/system/libexec/rime-firewall}"
SRC_CATALOGUE="${RIME_FW_CATALOGUE:-$ROOT/files/system/firewall/services}"

MUT_RULES=""
MUT_HELPER=""
SELFTEST=0
while [ $# -gt 0 ]; do
    case "$1" in
        --self-test)     SELFTEST=1; shift ;;
        --mutate-rules)  MUT_RULES="$2"; shift 2 ;;
        --mutate-helper) MUT_HELPER="$2"; shift 2 ;;
        *) printf 'usage: %s [--self-test | --mutate-rules SED | --mutate-helper SED]\n' "$0" >&2; exit 2 ;;
    esac
done

pass=0; fail=0; skipped=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s — %s\n' "$1" "${2:-}"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s — %s\n' "$1" "${2:-}"; skipped=$((skipped + 1)); }
sec()  { printf '\n── %s ──\n' "$1"; }

finish() {
    printf '\nrime firewall live: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skipped"
    [ "$fail" -eq 0 ]
}

# ═════════════════════════════════════════════════════════════════════════════
#  --self-test: every case, against a policy with one thing broken
# ═════════════════════════════════════════════════════════════════════════════
# Each entry is  which-file | sed expression | the case the mutant must redden.
# The case name is matched as a prefix, so a mutant that reddens the whole
# suite still has to redden the specific thing it was written for — which is
# the check that catches an assertion quietly testing nothing.
if [ "$SELFTEST" = 1 ]; then
    MUTANTS=(
"rules|/^ *ct state established,related accept$/d|an outbound connection open when the policy loads keeps working"
"rules|/^ *tcp dport 22 accept$/d|an ssh connection open when the policy loads keeps working"
"rules|s|^\( *\)tcp dport 22 accept$|\1tcp dport 22 accept\n\1tcp dport 9999 accept|;|a new connection to a port nobody opened is dropped"
"rules|/^ *iif lo accept$/d|the machine can still talk to itself"
"rules|/^ *udp sport 67 udp dport 68 accept$/d|a DHCP offer reaches the client"
"rules|/^ *ip6 daddr fe80::\/10 udp dport 546 accept$/d|a DHCPv6 reply reaches the client"
"rules|s/^\( *\)ip protocol icmp icmp type { echo-request }.*/\1# mutant/|ping answers from outside"
"rules|s/^\( *\)ip6 nexthdr icmpv6 icmpv6 type { echo-request }.*/\1# mutant/|ping6 answers from outside"
"rules|s/nd-router-advert, nd-neighbor-solicit, nd-neighbor-advert, nd-redirect,/nd-router-advert, nd-redirect,/|IPv6 neighbour discovery still resolves this machine"
"rules|/^ *ip daddr 224.0.0.251 udp dport 5353 accept$/d|mDNS is still answered"
"rules|/^ *tcp dport @allowed_tcp accept$/d|allow really opens a port against the live ruleset"
"helper|s/^\( *\)nft add element/\1: skipped-by-mutant nft add element/|allow really opens a port against the live ruleset"
"helper|s/^\( *\)rm -f \"\${CONF_DIR}\/\${name}.conf\"$/\1: mutant kept the file/|deny really closes it again"
"helper|s/^\( *\)if apply_sets; then$/\1if true; then/|the exceptions come back after a policy reload"
"helper|s/rejected=\$((rejected + 1))/return 1/|one malformed exception does not take the others with it"
"rules|/^ *ct state established,related accept$/d;/destination-unreachable, time-exceeded, parameter-problem/d|path MTU discovery still works"
"rules|/^ *table inet rime$/d;/^delete table inet rime$/d|loading the policy twice does not double the rules"
"rules|s|^    chain output {|    chain forward {\n        type filter hook forward priority filter; policy drop;\n    }\n\n    chain output {|;|traffic another table accepts is still forwarded"
"rules|s|^\( *\)ct state invalid drop$|\1tcp dport 9999 accept\n\1ct state invalid drop|;|a connection on a closed port does not survive the policy loading"
    )
    printf 'rime firewall live — self-test over %d mutants\n' "${#MUTANTS[@]}"
    printf 'Each mutant must make the named case FAIL. A mutant the suite survives\n'
    printf 'is an assertion that is not testing what its name says.\n\n'

    base_out="$("$0" 2>&1)"; base_rc=$?
    base_line="$(printf '%s' "$base_out" | tail -1)"
    printf 'baseline (nothing broken): %s\n' "$base_line"
    if [ "$base_rc" != 0 ]; then
        printf '\nthe baseline itself fails; fix that before reading the mutants\n%s\n' "$base_out"
        exit 1
    fi

    caught=0; missed=0; n=0
    for m in "${MUTANTS[@]}"; do
        n=$((n + 1))
        which="${m%%|*}"; rest="${m#*|}"
        expr="${rest%|*}"; want="${rest##*|}"
        if [ "$which" = rules ]; then
            out="$("$0" --mutate-rules "$expr" 2>&1)"
        else
            out="$("$0" --mutate-helper "$expr" 2>&1)"
        fi
        line="$(printf '%s' "$out" | tail -1)"
        if printf '%s' "$out" | pipe_has -F "FAIL  $want"; then
            printf '  %2d caught  %-58s %s\n' "$n" "$want" "$line"
            caught=$((caught + 1))
        else
            printf '  %2d MISSED  %-58s %s\n' "$n" "$want" "$line"
            printf '      mutant: %s %s\n' "$which" "$expr"
            missed=$((missed + 1))
        fi
    done
    printf '\nself-test: %d/%d mutants caught, %d missed\n' "$caught" "$((caught + missed))" "$missed"
    [ "$missed" -eq 0 ]
    exit $?
fi

# ═════════════════════════════════════════════════════════════════════════════
#  Preconditions
# ═════════════════════════════════════════════════════════════════════════════
for f in "$SRC_RULES" "$SRC_HELPER" "$SRC_CATALOGUE"; do
    [ -r "$f" ] || { printf 'cannot read %s\n' "$f" >&2; exit 2; }
done
for t in ip nft socat ping; do
    if ! command -v "$t" >/dev/null 2>&1; then
        skip "the whole suite" "$t is not installed"
        finish; exit $?
    fi
done
if [ "$(id -u)" != 0 ]; then
    skip "the whole suite" "network namespaces need root; run with sudo"
    finish; exit $?
fi
if ! ip netns list >/dev/null 2>&1; then
    skip "the whole suite" "this kernel has no network namespaces"
    finish; exit $?
fi

W="$(mktemp -d /tmp/rime-fw-live.XXXXXX)" || exit 2
NS_HOST=rimefw-host; NS_NET=rimefw-net; NS_FAR=rimefw-far; NS_LAN=rimefw-lan

cleanup() {
    local rc=$?
    for n in "$NS_HOST" "$NS_NET" "$NS_FAR" "$NS_LAN"; do
        ip netns pids "$n" 2>/dev/null | xargs -r kill -9 2>/dev/null
        ip netns del "$n" 2>/dev/null
    done
    rm -rf "$W"
    [ "${MADE_ETC_RIME:-0}" = 1 ] && rmdir /etc/rime 2>/dev/null
    return $rc
}
trap cleanup EXIT INT TERM

# The copies actually loaded. Mutants edit these, never the tree.
RULES="$W/rime.nft"; HELPER="$W/rime-firewall"; CATALOGUE="$W/services"
cp "$SRC_RULES" "$RULES"; cp "$SRC_HELPER" "$HELPER"; cp "$SRC_CATALOGUE" "$CATALOGUE"
chmod +x "$HELPER"
[ -n "$MUT_RULES" ]  && { sed -i "$MUT_RULES"  "$RULES";  printf 'MUTANT rules:  %s\n' "$MUT_RULES"; }
[ -n "$MUT_HELPER" ] && { sed -i "$MUT_HELPER" "$HELPER"; printf 'MUTANT helper: %s\n' "$MUT_HELPER"; }

# ═════════════════════════════════════════════════════════════════════════════
#  The topology
# ═════════════════════════════════════════════════════════════════════════════
# `timeout` cannot run a shell function, so every probe below that needs a
# deadline calls $IP directly. The first version of this file wrapped a
# function and got 127 from every probe, which reads exactly like a DROP —
# nine cases "failed" against a policy that was doing nothing of the kind.
IP="$(command -v ip)"
nsx()  { "$IP" netns exec "$1" "${@:2}"; }
nsxt() { timeout "$1" "$IP" netns exec "$2" "${@:3}"; }
h() { nsx "$NS_HOST" "$@"; }
n() { nsx "$NS_NET"  "$@"; }
l() { nsx "$NS_LAN"  "$@"; }

build_topology() {
    for ns in "$NS_HOST" "$NS_NET" "$NS_FAR" "$NS_LAN"; do
        ip netns del "$ns" 2>/dev/null
        ip netns add "$ns" || return 1
        ip -n "$ns" link set lo up
    done

    ip link add hn type veth peer name nh                 || return 1
    ip link set hn netns "$NS_HOST"; ip link set nh netns "$NS_NET"
    ip link add nf type veth peer name fn                 || return 1
    ip link set nf netns "$NS_NET";  ip link set fn netns "$NS_FAR"
    ip link add hl type veth peer name lh                 || return 1
    ip link set hl netns "$NS_HOST"; ip link set lh netns "$NS_LAN"

    ip -n "$NS_HOST" addr add 10.9.44.2/24 dev hn
    ip -n "$NS_HOST" addr add fd44::2/64   dev hn nodad
    ip -n "$NS_HOST" addr add 10.9.46.1/24 dev hl
    ip -n "$NS_NET"  addr add 10.9.44.1/24 dev nh
    ip -n "$NS_NET"  addr add fd44::1/64   dev nh nodad
    ip -n "$NS_NET"  addr add 10.9.45.1/24 dev nf
    ip -n "$NS_FAR"  addr add 10.9.45.2/24 dev fn
    ip -n "$NS_LAN"  addr add 10.9.46.2/24 dev lh

    # The small link. 1280 is IPv6's floor, so it is a size a real path really
    # can be, and it is below the 1400-byte probe below.
    ip -n "$NS_NET" link set nf mtu 1280
    ip -n "$NS_FAR" link set fn mtu 1280

    for spec in "$NS_HOST hn" "$NS_NET nh" "$NS_NET nf" "$NS_FAR fn" "$NS_HOST hl" "$NS_LAN lh"; do
        set -- $spec; ip -n "$1" link set "$2" up
    done

    ip -n "$NS_HOST" route add default via 10.9.44.1
    ip -n "$NS_HOST" -6 route add default via fd44::1 2>/dev/null
    ip -n "$NS_FAR"  route add default via 10.9.45.1
    ip -n "$NS_LAN"  route add default via 10.9.46.1
    ip -n "$NS_NET"  route add 10.9.46.0/24 via 10.9.44.2
    n  sysctl -qw net.ipv4.ip_forward=1
    h  sysctl -qw net.ipv4.ip_forward=1

    # Link-local addresses take a moment to leave tentative. Everything below
    # that touches IPv6 needs them, and a race here reads as "IPv6 is broken".
    local i=0
    while [ $i -lt 50 ]; do
        ip -n "$NS_HOST" -6 addr show dev hn scope link 2>/dev/null | pipe_has 'inet6.*fe80' &&
        ip -n "$NS_NET"  -6 addr show dev nh scope link 2>/dev/null | pipe_has 'inet6.*fe80' && break
        sleep 0.1; i=$((i + 1))
    done
    LL_HOST="$(ip -n "$NS_HOST" -6 addr show dev hn scope link | awk '/inet6/{print $2}' | cut -d/ -f1 | head -1)"
    LL_NET="$(ip  -n "$NS_NET"  -6 addr show dev nh scope link | awk '/inet6/{print $2}' | cut -d/ -f1 | head -1)"
    [ -n "$LL_HOST" ] && [ -n "$LL_NET" ]
}

build_topology || { printf 'could not build the namespaces\n' >&2; exit 2; }

# ═════════════════════════════════════════════════════════════════════════════
#  Loading, and the little primitives every case is written in
# ═════════════════════════════════════════════════════════════════════════════
load_policy()  { h nft -f "$RULES" 2>&1; }
unload_policy() { h nft flush ruleset 2>/dev/null; }
policy_loaded() { h nft list table inet rime >/dev/null 2>&1; }

# Reachability from the network side. rc 0 = the handshake completed;
# anything else = it did not, and a timeout is what a DROP looks like.
tcp_reach() { # ns addr port [timeout] → rc 0 when the handshake completed.
    local ns="$1" addr="$2" port="$3" t="${4:-3}"
    nsxt "$t" "$ns" socat -u /dev/null "TCP4:$addr:$port,connect-timeout=$t" >/dev/null 2>&1
}

listen_tcp() { # ns port  → echoes back whatever it is sent
    nsx "$1" socat "TCP4-LISTEN:$2,reuseaddr,fork" PIPE >/dev/null 2>&1 &
    sleep 0.4
}
listen_udp() { # ns port outfile
    nsx "$1" socat -u "UDP4-RECV:$2" "CREATE:$3" >/dev/null 2>&1 &
    sleep 0.4
}

udp_send() { # ns dst port payload [src-ip:src-port]
    local ns="$1" addr="$2" port="$3" pay="$4" bind="${5:-}"
    # socat accepts `sp=` and then ignores it — measured with an nft counter:
    # the datagram left with an ephemeral source port and the sport rule it was
    # meant to exercise never matched. `bind=` is the option that works.
    local opts=""; [ -n "$bind" ] && opts=",bind=$bind"
    printf '%s' "$pay" | nsxt 3 "$ns" socat -u - "UDP4-DATAGRAM:$addr:$port$opts" >/dev/null 2>&1
}

# ═════════════════════════════════════════════════════════════════════════════
sec "the policy loads at all"
# ═════════════════════════════════════════════════════════════════════════════
out="$(load_policy)"; rc=$?
if [ "$rc" = 0 ] && policy_loaded; then
    ok "the shipped policy loads into a kernel"
else
    bad "the shipped policy loads into a kernel" "${out:-nft exited $rc}"
    finish; exit $?
fi

got="$(h nft list chain inet rime input | grep -c 'policy drop' || true)"
[ "$got" -ge 1 ] && ok "input really is policy drop once loaded" \
                 || bad "input really is policy drop once loaded" "the loaded chain does not say drop"

# The file declares `table inet rime { … }`. nft MERGES such a block into an
# existing table rather than replacing it, so without the delete at the top of
# the file a second load appends every rule again — and a doubled ruleset is
# how a `limit rate` rule silently becomes twice the rate it says.
before="$(h nft list chain inet rime input | grep -c 'accept' || true)"
load_policy >/dev/null
after="$(h nft list chain inet rime input | grep -c 'accept' || true)"
if [ "$before" = "$after" ]; then
    ok "loading the policy twice does not double the rules"
else
    bad "loading the policy twice does not double the rules" "$before accepts became $after"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "connections that already exist when the policy arrives"
# ═════════════════════════════════════════════════════════════════════════════
# The one that would cost a machine, and it does not have one answer. conntrack
# is not even loaded on a stock Rime box, so a connection open when the policy
# loads is one conntrack has never seen. It has no entry to call ESTABLISHED,
# and the first packet of a brand-new entry is `new` — which the policy drops.
#
# So `ct state established,related accept` does NOT rescue a pre-existing
# connection. What rescues each kind is different, and the difference is worth
# three cases rather than one, because it decides whether ssh survives:
#
#   inbound to an open port    the port's own rule carries it — this is why
#                              `systemctl start rime-firewall` over ssh is safe
#   outbound                   our next packet out creates the conntrack entry,
#                              and the reply to it is ESTABLISHED
#   inbound to a closed port   nothing carries it, and it dies. Correct, and
#                              the reason the policy loads before the network
#                              at boot rather than after it.
survives() { # ns_server port ns_client server_ip → 0 if the connection lived
    local sns="$1" port="$2" cns="$3" sip="$4"
    unload_policy
    rm -f "$W/p1" "$W/go" "$W/r1" "$W/r2"
    listen_tcp "$sns" "$port"
    nsxt 30 "$cns" bash -c '
        exec 3<>/dev/tcp/'"$sip"'/'"$port"' || exit 1
        printf "before\n" >&3
        IFS= read -r -t 5 -u 3 line && printf "%s" "$line" > '"$W"'/r1
        : > '"$W"'/p1
        i=0; while [ ! -e '"$W"'/go ] && [ $i -lt 200 ]; do sleep 0.1; i=$((i+1)); done
        printf "after\n" >&3
        IFS= read -r -t 6 -u 3 line && printf "%s" "$line" > '"$W"'/r2
    ' >/dev/null 2>&1 &
    local cpid=$! i=0
    while [ ! -e "$W/p1" ] && [ $i -lt 120 ]; do sleep 0.1; i=$((i + 1)); done
    load_policy >/dev/null
    : > "$W/go"
    wait "$cpid" 2>/dev/null
    [ "$(cat "$W/r1" 2>/dev/null)" = before ] || { printf '      (the harness never got a first byte)\n'; return 2; }
    [ "$(cat "$W/r2" 2>/dev/null)" = after ]
}

survives "$NS_HOST" 22 "$NS_NET" 10.9.44.2
case $? in
    0) ok "an ssh connection open when the policy loads keeps working" ;;
    2) bad "an ssh connection open when the policy loads keeps working" "the harness broke before the policy loaded" ;;
    *) bad "an ssh connection open when the policy loads keeps working" "loading the policy would strand a remote developer" ;;
esac

survives "$NS_NET" 8081 "$NS_HOST" 10.9.44.1
case $? in
    0) ok "an outbound connection open when the policy loads keeps working" ;;
    2) bad "an outbound connection open when the policy loads keeps working" "the harness broke before the policy loaded" ;;
    *) bad "an outbound connection open when the policy loads keeps working" "every browser tab would stall" ;;
esac

# The half without which the two above prove nothing: on a port the policy does
# not open, a pre-existing connection does NOT survive and a new one is
# dropped. If either of these passed, the policy would be a no-op.
survives "$NS_HOST" 9999 "$NS_NET" 10.9.44.2
if [ $? = 0 ]; then
    bad "a connection on a closed port does not survive the policy loading" "it lived; the policy is not enforcing"
else
    ok "a connection on a closed port does not survive the policy loading"
fi

load_policy >/dev/null
listen_tcp "$NS_HOST" 9999
if tcp_reach "$NS_NET" 10.9.44.2 9999; then
    bad "a new connection to a port nobody opened is dropped" "it connected; the policy is not enforcing"
else
    ok "a new connection to a port nobody opened is dropped"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "the machine does not look broken"
# ═════════════════════════════════════════════════════════════════════════════
# NOT a ping: an echo-request to 127.0.0.1 is accepted by the ICMP rule
# whether or not loopback is allowed, so a ping here passed against a policy
# with `iif lo accept` deleted. What the lo rule actually carries is the
# seventeen listening ports a desktop session has on 127.0.0.1, none of which
# are in the accept list — so the probe is a TCP connection to one of them.
listen_tcp "$NS_HOST" 4444
if nsxt 3 "$NS_HOST" socat -u /dev/null "TCP4:127.0.0.1:4444,connect-timeout=3" >/dev/null 2>&1; then
    ok "the machine can still talk to itself"
else
    bad "the machine can still talk to itself" "loopback is filtered; most of a desktop session is on it"
fi

# A name lookup is a UDP round trip to somebody else's port 53. What has to
# work is that the ANSWER gets back in, which is conntrack's job, not a rule's.
n socat -T3 UDP4-RECVFROM:53,fork PIPE >/dev/null 2>&1 &
sleep 0.3
reply="$(printf 'query' | nsxt 4 "$NS_HOST" socat -T2 - UDP4:10.9.44.1:53 2>/dev/null)"
[ "$reply" = "query" ] && ok "a reply to our own outbound query comes back" \
                       || bad "a reply to our own outbound query comes back" "got '${reply:-nothing}'"

n socat -T3 TCP4-LISTEN:80,reuseaddr,fork PIPE >/dev/null 2>&1 &
sleep 0.3
reply="$(printf 'get' | nsxt 4 "$NS_HOST" socat -T2 - TCP4:10.9.44.1:80 2>/dev/null)"
[ "$reply" = "get" ] && ok "an outbound TCP connection still completes" \
                     || bad "an outbound TCP connection still completes" "got '${reply:-nothing}'"

# DHCP renewal. The offer arrives before any connection exists, so conntrack
# cannot call it related and only the explicit rule lets it in.
rm -f "$W/dhcp"
listen_udp "$NS_HOST" 68 "$W/dhcp"
udp_send "$NS_NET" 10.9.44.2 68 "OFFER" 10.9.44.1:67
sleep 0.5
grep -q OFFER "$W/dhcp" 2>/dev/null \
    && ok "a DHCP offer reaches the client" \
    || bad "a DHCP offer reaches the client" "a lease would never renew"

rm -f "$W/dhcp6"
h socat -u "UDP6-RECV:546" "CREATE:$W/dhcp6" >/dev/null 2>&1 &
sleep 0.3
printf 'REPLY6' | nsxt 3 "$NS_NET" socat -u - "UDP6-DATAGRAM:[$LL_HOST%nh]:546,bind=[$LL_NET%nh]:547" >/dev/null 2>&1
sleep 0.5
grep -q REPLY6 "$W/dhcp6" 2>/dev/null \
    && ok "a DHCPv6 reply reaches the client" \
    || bad "a DHCPv6 reply reaches the client" "a v6 lease would never renew"

h ping -c2 -W2 10.9.44.1 >/dev/null 2>&1 \
    && ok "ping still works outbound" \
    || bad "ping still works outbound" "echo replies are being dropped"
n ping -c2 -W2 10.9.44.2 >/dev/null 2>&1 \
    && ok "ping answers from outside" \
    || bad "ping answers from outside" "the machine is invisible to a diagnostic everyone reaches for first"

h ping -6 -c2 -W2 fd44::1 >/dev/null 2>&1 \
    && ok "ping6 still works outbound" \
    || bad "ping6 still works outbound" "echo replies are being dropped"
n ping -6 -c2 -W2 fd44::2 >/dev/null 2>&1 \
    && ok "ping6 answers from outside" \
    || bad "ping6 answers from outside" "the machine is invisible over v6"

# Neighbour discovery is the difference between "protected" and "no network".
# Emptying the cache on both sides forces a real NS/NA exchange rather than
# reading back an entry the ping above already made.
h ip -6 neigh flush all 2>/dev/null
n ip -6 neigh flush all 2>/dev/null
if nsxt 8 "$NS_NET" ping -6 -c2 -W2 "$LL_NET%nh" >/dev/null 2>&1 && \
   nsxt 8 "$NS_NET" ping -6 -c2 -W2 "$LL_HOST%nh" >/dev/null 2>&1; then
    nd="$(n ip -6 neigh show dev nh | grep -c "$LL_HOST" || true)"
    [ "$nd" -ge 1 ] && ok "IPv6 neighbour discovery still resolves this machine" \
                    || bad "IPv6 neighbour discovery still resolves this machine" "no neighbour entry was formed"
else
    bad "IPv6 neighbour discovery still resolves this machine" "the link-local ping never got an answer"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "path MTU discovery, which fails as slowness rather than as an error"
# ═════════════════════════════════════════════════════════════════════════════
# HOST → far crosses a 1280-byte link at `net`, so a 1400-byte packet with DF
# set has to come back as ICMP fragmentation-needed. If HOST drops that, the
# connection does not fail — it stalls, and nobody blames the firewall.
pmtu_works() {
    h ip route flush cache 2>/dev/null
    local out; out="$(nsxt 8 "$NS_HOST" ping -M 'do' -s 1400 -c2 -W2 10.9.45.2 2>&1)"
    printf '%s' "$out" | pipe_has -i 'frag.*needed\|mtu *= *1280' && return 0
    h ip route get 10.9.45.2 2>/dev/null | pipe_has 'mtu 1280'
}
if pmtu_works; then
    ok "path MTU discovery still works"
else
    bad "path MTU discovery still works" "the ICMP fragmentation-needed never arrived"
fi

# Two rules can carry this — conntrack calls the ICMP error RELATED, and the
# explicit icmp rule accepts it on its own merits. That is defence in depth
# rather than redundancy, and it is worth knowing which half is load-bearing,
# because a future edit will delete one of them.
cp "$RULES" "$W/no-related.nft"
sed -i 's/^\( *\)ct state established,related accept$/\1ct state established accept/' "$W/no-related.nft"
unload_policy; h nft -f "$W/no-related.nft" >/dev/null 2>&1
pmtu_works && ok "PMTU survives losing conntrack's related, on the icmp rule alone" \
           || bad "PMTU survives losing conntrack's related, on the icmp rule alone" "neither half carries it"

cp "$RULES" "$W/no-icmperr.nft"
sed -i '/destination-unreachable, time-exceeded, parameter-problem/d' "$W/no-icmperr.nft"
unload_policy; h nft -f "$W/no-icmperr.nft" >/dev/null 2>&1
pmtu_works && ok "PMTU survives losing the icmp rule, on conntrack alone" \
           || bad "PMTU survives losing the icmp rule, on conntrack alone" "neither half carries it"

unload_policy; load_policy >/dev/null

# ═════════════════════════════════════════════════════════════════════════════
sec "mDNS, which rime host and every printer depend on"
# ═════════════════════════════════════════════════════════════════════════════
# A real mDNS query goes to the group address, not to this host. RFC 6762 is
# multicast, which is why scoping the rule to 224.0.0.251 costs discovery
# nothing. The listener joins the group on `hn` explicitly; without the
# membership the kernel never delivers the datagram and the case would fail for
# a reason that has nothing to do with the firewall.
rm -f "$W/mdns"
nsx "$NS_HOST" socat -u "UDP4-RECV:5353,ip-add-membership=224.0.0.251:hn" "CREATE:$W/mdns" >/dev/null 2>&1 &
sleep 0.4
# Bind the source to the veth address: without it the kernel has no route for
# 224.0.0.0/4 in this namespace and the datagram never leaves.
udp_send "$NS_NET" 224.0.0.251 5353 "QUERY" 10.9.44.1:5353
sleep 0.5
grep -q QUERY "$W/mdns" 2>/dev/null \
    && ok "mDNS is still answered" \
    || bad "mDNS is still answered" "the machine cannot be found by name on its own LAN"

# The other half of the same decision, and the reason the rule was narrowed.
# A unicast datagram to port 5353 is not how a printer is found; it is how a
# stranger enumerates this machine and how an mDNS reflector recruits it.
rm -f "$W/mdns-uni"
listen_udp "$NS_HOST" 5353 "$W/mdns-uni"
udp_send "$NS_NET" 10.9.44.2 5353 "UNICAST"
sleep 0.5
grep -q UNICAST "$W/mdns-uni" 2>/dev/null \
    && bad "a unicast probe to 5353 is refused" "the port is open to any host, not just the mDNS group" \
    || ok "a unicast probe to 5353 is refused"

# LLMNR is not accepted at all. systemd-resolved ships LLMNR=resolve, which
# asks and never answers, so the old accept admitted packets to a service that
# discards them. Asserted rather than left implied, so re-adding the rule
# without re-deciding the policy turns this red.
rm -f "$W/llmnr"
listen_udp "$NS_HOST" 5355 "$W/llmnr"
udp_send "$NS_NET" 10.9.44.2 5355 "LLMNR"
sleep 0.5
grep -q LLMNR "$W/llmnr" 2>/dev/null \
    && bad "LLMNR is not reachable from the network" "5355 is open, and nothing on this machine answers it" \
    || ok "LLMNR is not reachable from the network"

# ═════════════════════════════════════════════════════════════════════════════
sec "allow and deny, against the live ruleset rather than the config file"
# ═════════════════════════════════════════════════════════════════════════════
# The helper reads /etc/rime/firewall.d and /usr/share/rime/firewall/services.
# Both are put under this run's own directory inside a private mount namespace,
# so nothing here writes to the machine's real /etc.
ETCDIR="$W/etc-rime"; mkdir -p "$ETCDIR/firewall.d"

# /etc/rime may not exist yet on a machine that has never run this; create it
# for real and take it away again, so the run leaves nothing behind.
MADE_ETC_RIME=0
[ -d /etc/rime ] || { mkdir -p /etc/rime 2>/dev/null && MADE_ETC_RIME=1; }

fwctl() { # the helper, in HOST's netns, with the test config mounted over the
          # real paths inside a private mount namespace. /usr is read-only
          # under a sysext on a real Rime machine, so the catalogue directory
          # is SHADOWED by a tmpfs rather than written into.
    nsx "$NS_HOST" unshare -m --propagation private -- \
        bash -c '
            mount --bind "$1" /etc/rime               2>/dev/null || exit 90
            mount -t tmpfs tmpfs /usr/share/rime      2>/dev/null || exit 91
            mkdir -p /usr/share/rime/firewall         2>/dev/null || exit 91
            cp "$2" /usr/share/rime/firewall/services 2>/dev/null || exit 91
            shift 2
            exec "$@"
        ' _ "$ETCDIR" "$CATALOGUE" "$HELPER" "$@"
}

# /etc/rime and /usr/share/rime/firewall must exist as mount points. On a
# machine where /usr is read-only they cannot be created, and the bind fails —
# say which rather than reporting the helper broken.
if ! fwctl status >/dev/null 2>&1; then
    probe=$(fwctl status >/dev/null 2>&1; echo $?)
    case "$probe" in
        90) skip "allow really opens a port against the live ruleset" "cannot bind-mount /etc/rime" ;;
        91) skip "allow really opens a port against the live ruleset" "cannot shadow /usr/share/rime" ;;
        *)  skip "allow really opens a port against the live ruleset" "the helper exited $probe" ;;
    esac
    skip "deny really closes it again" "see above"
    skip "the exceptions come back after a policy reload" "see above"
    skip "one malformed exception leaves the base policy standing" "see above"
    skip "one malformed exception does not take the others with it" "see above"
else
    listen_tcp "$NS_HOST" 22000   # syncthing, in the shipped catalogue
    listen_tcp "$NS_HOST" 11434   # ollama, likewise

    if tcp_reach "$NS_NET" 10.9.44.2 22000; then
        bad "a catalogued service is closed until it is allowed" "22000 answered before anything opened it"
    else
        ok "a catalogued service is closed until it is allowed"
    fi

    out="$(fwctl allow syncthing 2>&1)"; rc=$?
    if [ "$rc" = 0 ] && tcp_reach "$NS_NET" 10.9.44.2 22000; then
        ok "allow really opens a port against the live ruleset"
    else
        bad "allow really opens a port against the live ruleset" "rc=$rc ${out}"
    fi

    got="$(h nft list set inet rime allowed_tcp 2>/dev/null | grep -c 22000 || true)"
    [ "$got" -ge 1 ] && ok "the port is in the live set, not only in the file" \
                     || bad "the port is in the live set, not only in the file" "allowed_tcp does not contain it"

    out="$(fwctl deny syncthing 2>&1)"; rc=$?
    if [ "$rc" = 0 ] && ! tcp_reach "$NS_NET" 10.9.44.2 22000; then
        ok "deny really closes it again"
    else
        bad "deny really closes it again" "rc=$rc ${out}; the port is still answering"
    fi

    # ── criterion 5: a reload must not lose what the user allowed ────────────
    fwctl allow syncthing >/dev/null 2>&1
    fwctl allow ollama    >/dev/null 2>&1
    # Exactly what `systemctl restart rime-firewall` runs, in order.
    h nft delete table inet rime 2>/dev/null
    load_policy >/dev/null
    fwctl reload >/dev/null 2>&1
    if tcp_reach "$NS_NET" 10.9.44.2 22000 && tcp_reach "$NS_NET" 10.9.44.2 11434; then
        ok "the exceptions come back after a policy reload"
    else
        bad "the exceptions come back after a policy reload" "a restart silently closed what the user opened"
    fi

    # ── criterion 6: a broken exception file cannot take the policy down ─────
    printf 'tcp notaport\n' > "$ETCDIR/firewall.d/broken.conf"
    fwctl reload >/dev/null 2>&1; reload_rc=$?

    why=""
    policy_loaded || why="the table is gone"
    chain="$(h nft list chain inet rime input 2>/dev/null)"
    printf '%s' "$chain" | pipe_has 'policy drop'        || why="$why; the chain no longer drops by default"
    printf '%s' "$chain" | pipe_has 'tcp dport 22 accept' || why="$why; ssh is no longer accepted"
    tcp_reach "$NS_NET" 10.9.44.2 9999 && why="$why; a closed port started answering"
    if [ -z "$why" ]; then
        ok "one malformed exception leaves the base policy standing"
    else
        bad "one malformed exception leaves the base policy standing" "${why#; }"
    fi

    # The sharper half. The architectural claim is about the BASE policy, but a
    # user with one typo losing every other port they opened is the same bug
    # wearing a smaller hat — and it fails silently, because the reload is
    # ExecStart=- in the unit and prints "exceptions reapplied" either way.
    if tcp_reach "$NS_NET" 10.9.44.2 22000 && tcp_reach "$NS_NET" 10.9.44.2 11434; then
        ok "one malformed exception does not take the others with it"
    else
        bad "one malformed exception does not take the others with it" "a typo in one .conf closed the other exceptions"
    fi

    if [ "$reload_rc" != 0 ]; then
        ok "a reload that rejected a file says so in its exit status"
    else
        bad "a reload that rejected a file says so in its exit status" "it exited 0 and printed success"
    fi

    # `broken` appears in the exception list either way, so matching the name
    # alone would pass against a tool that said nothing was wrong. What has to
    # be there is the fact that it was REJECTED.
    out="$(fwctl status 2>&1)"
    if printf '%s' "$out" | grep -Eqi 'broken.*(could not|rejected|not applied|ignored)|(could not|rejected|not applied|ignored).*broken'; then
        ok "status names the exception it could not apply"
    else
        bad "status names the exception it could not apply" "the user is told the port is open when it is not"
    fi

    rm -f "$ETCDIR/firewall.d"/*.conf
    fwctl reload >/dev/null 2>&1
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "anything behind this machine, which is every rootful container"
# ═════════════════════════════════════════════════════════════════════════════
# The policy used to carry a forward chain with `policy drop` and a comment
# saying container traffic was unaffected because podman "hooks at its own
# priority in its own table". The two cases below are what removed it.
#
# nftables evaluates EVERY base chain registered at a hook. An `accept` in one
# does not stop the others; a `drop` in any one of them is final. So a second
# table that accepts — which is exactly what netavark, libvirt and incus each
# install — cannot rescue traffic this table drops, and a rootful container
# lost its network with nothing in its own rules to explain why.
listen_tcp "$NS_LAN" 8080
unload_policy
if tcp_reach "$NS_NET" 10.9.46.2 8080 4; then
    fwd_baseline=1; ok "the harness can forward through this machine at all"
else
    fwd_baseline=0; bad "the harness can forward through this machine at all" "routing is broken; the next case would be meaningless"
fi
load_policy >/dev/null

if [ "$fwd_baseline" = 1 ]; then
    # The stand-in for netavark: another table, its own base chain at the same
    # hook, accepting everything it sees.
    h nft -f - >/dev/null 2>&1 <<'STUB'
table inet rimefw_stub {
    chain forward {
        type filter hook forward priority filter; policy accept;
        accept
    }
}
STUB
    if tcp_reach "$NS_NET" 10.9.46.2 8080 4; then
        ok "traffic another table accepts is still forwarded"
    else
        bad "traffic another table accepts is still forwarded" "every rootful container on this machine has just lost its network"
    fi
    h nft delete table inet rimefw_stub 2>/dev/null
else
    skip "traffic another table accepts is still forwarded" "no forwarding baseline"
fi

finish
