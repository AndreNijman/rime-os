# ─────────────────────────────────────────────────────────────────────────────
#  network-loss — the registry is unreachable while the machine is asked
#  whether its image was signed.
#
#  P1-062 criterion 1's "network loss". The interesting subject is not a
#  download — a download that fails is obvious. It is `apex trust --verify`,
#  which answers a SECURITY question by contacting a registry, because there
#  the failure mode is a verdict rather than an error message: a machine that
#  reports "no signature" when the truth is "nobody could ask" has told its
#  owner their operating system is unverified on the evidence of a dropped
#  packet.
#
#  apexd/apex/src/verify.rs already makes that distinction — `Verdict::Absent`
#  is "the registry answered and holds no such artifact", `CouldNotRun` is
#  "verification did not run to a conclusion" — and this case is the end-to-end
#  proof that the distinction survives contact with a real dead network, rather
#  than only existing in the type.
#
#  INJECTION   the subject runs inside `unshare --user --map-root-user --net`:
#              a fresh network namespace whose only interface is a loopback
#              that is DOWN. Nothing is reachable, including localhost.
#  PROOF       a connect() attempt made by the HARNESS, inside the same
#              namespace, must fail. `unshare` exiting 0 says unshare ran;
#              python opening a socket to a routable address and getting
#              ENETUNREACH says the network is gone. On a kernel that refuses
#              an unprivileged user namespace the prerequisite probe catches it
#              first and the case reports could-not-inject — which is the
#              honest answer on a GitHub runner with
#              kernel.apparmor_restrict_unprivileged_userns=1.
#  SUBJECT     `apex trust --verify` against a fixture machine.
#  SURVIVAL    the report must say the registry could not be reached, must NOT
#              say the image is unsigned or that no signature exists, and must
#              leave the machine tree untouched.
# ─────────────────────────────────────────────────────────────────────────────

CASE_TITLE="the registry unreachable while verifying the booted image"
CASE_CRITERION="1 (network loss), 3 (diagnostics, no silent corruption)"
CASE_NEEDS="apex-binary userns netns"

case_setup() {
    chaos_mk_trust_root "$CASE_ROOT" "ghcr.io/andrenijman/apex-os:edge"
    echo "fixture machine at $CASE_ROOT"
}

# The subject, with and without the namespace. Both write their JSON to a file
# so the assertions read a document; `apex trust --verify --json` is what APEX
# Settings renders.
_verify() {
    APEX_TRUST_ROOT="$CASE_ROOT" "$APEX_BIN" trust --verify 2>&1
}

case_baseline() {
    # Deliberately NOT run inside the namespace: this is the control, and it is
    # allowed to reach the network. It is also allowed to fail — this machine
    # may have no route to ghcr.io either — which is why nothing below asserts
    # that the baseline succeeded, only that the two differ.
    _verify || true
}

case_inject() {
    # Nothing is injected here in the sense of changing the tree. The fault is
    # the environment the subject is run in, and it is applied in case_observe.
    # Stated rather than left implicit: a reader of this file must not go
    # looking for the mutation that is not there.
    echo "the fault is a network namespace, applied around the subject in case_observe"
}

case_prove() {
    command -v python3 >/dev/null 2>&1 || {
        echo "no python3, so the harness cannot prove the network is gone"; return 1; }
    # The independent observation, made in the SAME kind of namespace the
    # subject will run in. A TCP connect to a routable address must fail; a
    # connect that succeeded would mean the namespace leaked and the case is
    # about to test nothing.
    local out
    out="$(unshare --user --map-root-user --net -- python3 -c '
import socket, sys
s = socket.socket(); s.settimeout(3)
try:
    s.connect(("140.82.121.4", 443))          # a routable address, not a name
    print("CONNECTED")
except OSError as e:
    print("refused: %s" % e.strerror)
' 2>&1)" || true
    if [[ "$out" == *CONNECTED* ]]; then
        echo "a connect() succeeded inside the namespace; the network was not removed"
        return 1
    fi
    # DNS too, because a subject that resolves a name and then fails is in a
    # different state from one that cannot resolve at all, and the diagnostics
    # should say which.
    local dns
    dns="$(unshare --user --map-root-user --net -- python3 -c '
import socket
try:
    socket.gethostbyname("ghcr.io"); print("RESOLVED")
except OSError as e:
    print("no resolver: %s" % e)
' 2>&1)" || true
    echo "inside the namespace: connect -> $out ; dns -> $dns"
    return 0
}

case_observe() {
    # The namespace wraps the subject and nothing else. APEX_TRUST_ROOT is
    # exported into it explicitly: `unshare` preserves the environment, but the
    # driver refuses a subject invocation with no fixture root and the refusal
    # must be able to see the variable.
    APEX_TRUST_ROOT="$CASE_ROOT" unshare --user --map-root-user --net -- \
        env APEX_TRUST_ROOT="$CASE_ROOT" "$APEX_BIN" trust --verify 2>&1
}

case_judge() {
    # The claim that must not be made. "unsigned" is a registry answer — the
    # registry said it holds no signature — and a machine with no network has
    # not heard from any registry. apexd/apex/src/trust.rs already refuses to
    # use the word offline; this asserts it end to end, under a real dead
    # network rather than a fixture that pretends to be one.
    expect_silent_about "a machine that could not ask never reports the image as unsigned" \
        "unsigned" "$CASE_OBSERVED"
    expect_silent_about "…nor claims nobody signed it" \
        "nobody signed" "$CASE_OBSERVED"
    # And the claim that must be made: something has to name the failure to
    # reach the registry, or the report is a shrug.
    if grep -qiE 'could not|unreachable|not be reached|network|no route|temporary failure|resolve' \
        <<<"$CASE_OBSERVED"; then
        _chaos_pass "the report names a failure to reach the registry"
    else
        _chaos_fail "the report says nothing about why verification did not happen"
    fi
    expect_differs_from_baseline "the verdict under a dead network differs from the one with a network" \
        "$CASE_BASELINE_OUT" "$CASE_OBSERVED"
    expect_no_corruption "a failed verification changed nothing on the machine" \
        "$CASE_DIR/state.before" "$CASE_DIR/state.after"
}
