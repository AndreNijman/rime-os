# ─────────────────────────────────────────────────────────────────────────────
#  power-loss-during-update — the update note, torn in half.
#
#  P1-062 criterion 1's first named fault, aimed at the one piece of state an
#  `apex update` leaves behind on the machine itself.
#
#  ═══ WHY THIS FILE AND NOT THE OSTREE DEPLOYMENT ═══
#
#  "Power loss during update" has two halves. The ostree half — a staged
#  deployment discarded by a crash before clean shutdown, the machine coming
#  back on the old image — is what `chaos-loop` exercises in a VM, because it
#  needs a bootloader and a real reboot and cannot be faked in a fixture tree.
#
#  This is the OTHER half, and it is the half that is reachable in two seconds
#  on any machine: `apex update` writes
#  /var/lib/apex/channel/last-update.json, and that write is a plain
#  `std::fs::write` — no temp file, no rename. A power loss during it leaves a
#  file that exists, is the right length or shorter, and is not JSON.
#
#  What that file feeds is §26's update health gate: the next `apex update`
#  reads it to decide whether this machine has just come back from an update
#  that broke it. A file the machine cannot parse therefore disarms the gate,
#  and the surface that reports on it has to say so — "nothing recorded yet"
#  after a torn write is the same defect this project files under "permission
#  denied is not absence", on the screen somebody reads while their machine is
#  misbehaving.
#
#  ═══ WHAT IS INJECTED, AND WHAT IS ONLY SIMULATED ═══
#
#  Stated plainly because a harness that blurs this is worthless: the fault
#  injected here is the STATE A CRASH LEAVES, not the crash. The bytes are
#  truncated at a point chosen from the case's seed, which is exactly what a
#  torn `write(2)` to a page-cached file produces after the power goes; nothing
#  here cuts power to anything.
#
#  The proof is independent of the injector in the way that matters: the
#  harness re-reads the file and requires that a JSON parser refuse it and that
#  what remains be a strict prefix of what was written. `truncate` exiting 0
#  proves neither.
#
#  SUBJECT     `apex channel status`, the surface that reports how the last
#              update went, and `--json`, which is what APEX Settings reads.
#  SURVIVAL    the machine must say the record is unreadable. It must not say
#              "nothing recorded yet", because an update DID run. It must not
#              crash, and it must not rewrite or delete the damaged file behind
#              the user's back — a diagnostic destroyed by the diagnostic tool
#              is the corruption clause of criterion 3.
# ─────────────────────────────────────────────────────────────────────────────

CASE_TITLE="the update record torn mid-write by a power loss"
CASE_CRITERION="1 (power loss during update), 3 (diagnostics, no silent corruption)"
CASE_NEEDS="apex-binary"

RECORD=""
ORIG_BYTES=0

case_setup() {
    chaos_mk_trust_root "$CASE_ROOT"
    # A record left by an update that has NOT been rebooted into: from_digest
    # equals the booted digest, which is the state a machine is in between
    # `apex update` and the reboot — the exact window a power loss falls in.
    chaos_write_update_record "$CASE_ROOT" "$CHAOS_DIGEST"
    RECORD="$CASE_ROOT/var/lib/apex/channel/last-update.json"
    ORIG_BYTES="$(wc -c < "$RECORD")"
    [[ "$ORIG_BYTES" -gt 20 ]] || { echo "record is only $ORIG_BYTES bytes" >&2; return 1; }
    echo "record: $ORIG_BYTES bytes at $RECORD"
}

# The plain report is the diagnostic; the JSON goes to its own file so the
# assertions read a document instead of grepping prose. The first version of
# this case grepped for "could not be read" and PASSED on a subject that had
# said nothing of the kind — the phrase came from an unrelated row about
# package extensions. A false pass in a chaos harness is the worst outcome
# available, so the prose assertion is kept only for the one sentence a person
# has to see, and everything else reads the document.
_surface() {
    local tag="$1"
    APEX_TRUST_ROOT="$CASE_ROOT" "$APEX_BIN" channel status 2>&1
    local rc=$?
    APEX_TRUST_ROOT="$CASE_ROOT" "$APEX_BIN" channel status --json \
        > "$CASE_DIR/$tag.json" 2>/dev/null || true
    printf '\n--- channel status --json is in %s.json ---\n' "$tag"
    return "$rc"
}

case_baseline() { _surface baseline || true; }

case_inject() {
    # The cut point comes from the case's seeded stream, so --replay tears the
    # file at the same byte. Bounded away from 0 (an empty file is a different
    # fault — it parses as "absent" to a reader that stats rather than parses)
    # and away from the full length (which is not a tear at all).
    local cut
    cut="$(chaos_rand 1 $((ORIG_BYTES - 2)))"
    cp "$RECORD" "$CASE_DIR/record.orig"
    truncate -s "$cut" "$RECORD"
    # The exact bytes the subject is about to be shown, so "the report left the
    # evidence alone" can be asserted against a digest rather than a length. A
    # same-length rewrite is the one way a file can change that a byte count
    # cannot see.
    sha256sum < "$RECORD" | cut -d' ' -f1 > "$CASE_DIR/record.torn.sha"
    echo "truncated $ORIG_BYTES -> $cut bytes"
}

case_prove() {
    # Two independent observations, neither of them the injector's exit code.
    local now
    now="$(wc -c < "$RECORD")"
    if [[ "$now" -ge "$ORIG_BYTES" ]]; then
        echo "the record is still $now bytes; nothing was torn"
        return 1
    fi
    # A parser must refuse it. python3 is in the base image and on every runner
    # this repository uses; if it is not here, the case cannot prove its fault
    # and says so rather than assuming.
    command -v python3 >/dev/null 2>&1 || {
        echo "no python3, so the harness cannot prove the file is unparseable"
        return 1
    }
    if python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$RECORD" 2>/dev/null; then
        echo "the truncated record still parses as JSON; that is not a torn write"
        return 1
    fi
    # And what is left must be a prefix of what was written, which is what
    # makes it a TEAR rather than a scribble.
    if ! cmp -s -n "$now" "$RECORD" "$CASE_DIR/record.orig"; then
        echo "the remaining bytes are not a prefix of the original"
        return 1
    fi
    echo "the record is $now of $ORIG_BYTES bytes, is a prefix of the original, and no parser accepts it"
    return 0
}

case_observe() { _surface observe; }

case_judge() {
    # The baseline is the control: with the record intact, the machine reports
    # it and has no error to report. Without this, every assertion below could
    # pass against a fixture the subject never read in the first place.
    expect_json "with the record intact the machine reports when the update ran" \
        "$CASE_DIR/baseline.json" 'd["lastUpdate"]["at"]' "1788700000"
    expect_json "…and has no error to report about it" \
        "$CASE_DIR/baseline.json" 'd["lastUpdateError"]' "None"

    # The honesty clause. An update ran; the note about it is damaged. A `null`
    # lastUpdate with a `null` error is the document saying "no update has ever
    # run here", which is false and is what APEX Settings would render.
    expect_json "a damaged record is not reported as no record" \
        "$CASE_DIR/observe.json" 'd["lastUpdate"]' "None"
    expect_json_nonempty "…the document carries the reason instead" \
        "$CASE_DIR/observe.json" 'd["lastUpdateError"]'
    # And the sentence a person actually reads. This one IS a prose assertion,
    # because the prose is the deliverable: "nothing recorded yet" on a machine
    # that updated an hour ago sends somebody looking in the wrong place.
    expect_silent_about "the report never says nothing was recorded" \
        "nothing recorded yet" "$CASE_OBSERVED"

    # The health gate fails OPEN on an unreadable record, by design — refusing
    # an update because a note is damaged would strand the machine the update
    # would fix. Asserted so that the forgiveness stays deliberate rather than
    # becoming accidental.
    expect_json "the update is not held by a record nobody can read" \
        "$CASE_DIR/observe.json" 'd["held"]' "False"

    expect_differs_from_baseline "the surface reacted to the fault at all" \
        "$CASE_BASELINE_OUT" "$CASE_OBSERVED"
    # The corruption clause: reading a broken machine must not break it
    # further. `apex channel status` is a report; the damaged file must still
    # be there, byte for byte, for whoever debugs this next.
    expect_no_corruption "the damaged record was left intact for a human to look at" \
        "$CASE_DIR/state.before" "$CASE_DIR/state.after" \
        '^[<>] [0-9a-f]{64} var/lib/apex/channel/last-update\.json$'
    local now_sha torn_sha
    torn_sha="$(cat "$CASE_DIR/record.torn.sha" 2>/dev/null || echo missing)"
    now_sha="$(sha256sum < "$RECORD" 2>/dev/null | cut -d' ' -f1 || echo gone)"
    if [[ "$now_sha" == "$torn_sha" ]]; then
        _chaos_pass "the torn record is byte-identical after the report ran"
    else
        _chaos_fail "the report rewrote or removed the damaged record ($torn_sha -> $now_sha)"
    fi
}
