# ─────────────────────────────────────────────────────────────────────────────
#  failed-rollback — the rollback target is there and cannot be read.
#
#  P1-062 criterion 1's last named fault. "Failed rollback" has two shapes and
#  only one of them is interesting:
#
#    * there IS no previous deployment. A machine that has never updated is in
#      that state, tests/test-apex-recover.sh already covers it, and the
#      recovery surface says "nothing to roll back to yet".
#    * there IS one and the machine cannot see it. A half-written ostree
#      stateroot, a deploy directory whose mode was lost, a filesystem that
#      came back from a crash with an unreadable inode. This is the one that
#      matters, because the WRONG answer to it — "only the booted deployment
#      exists" — tells a user in trouble that their way back does not exist
#      when it does.
#
#  That second shape is this repository's own recorded defect, "permission
#  denied is not absence", pointed at the one row where getting it wrong costs
#  somebody their machine.
#
#  INJECTION       chmod 0000 on /ostree/deploy/apex/deploy, the directory
#                  holding both deployments.
#  PROOF           the harness itself tries to list that directory and must be
#                  refused. This is independent of the injector: `chmod`
#                  exiting 0 says the mode call succeeded, not that a read is
#                  now denied — and on a tree owned by root, or under a
#                  filesystem mounted with no permission enforcement, it would
#                  not be. Running as root is the live example: root ignores
#                  the mode, the read succeeds, and the case correctly reports
#                  could-not-inject rather than a pass.
#  SUBJECT         `apex recover status`, the surface APEX Settings polls.
#  SURVIVAL        it must name the refused read, must NOT claim there is
#                  nothing to roll back to, and must not have written anything
#                  into the machine tree while answering.
# ─────────────────────────────────────────────────────────────────────────────

CASE_TITLE="a rollback target that exists and cannot be read"
CASE_CRITERION="1 (failed rollback), 3 (no silent corruption)"
CASE_NEEDS="apex-binary"

DEPLOY_DIR=""

case_setup() {
    chaos_mk_recover_root "$CASE_ROOT"
    DEPLOY_DIR="$CASE_ROOT/ostree/deploy/apex/deploy"
    # Assert the fixture is the interesting one before anything is broken: two
    # deployments, so "nothing to roll back to" is a false statement rather
    # than an unlucky one.
    local n
    n="$(find "$DEPLOY_DIR" -mindepth 1 -maxdepth 1 -type d | wc -l)"
    [[ "$n" -eq 2 ]] || { echo "fixture has $n deployments, wanted 2" >&2; return 1; }
    echo "fixture: 2 deployments under $DEPLOY_DIR"
}

# Both renderings, every time. The plain report is the diagnostic a human
# reads out of the bundle; the JSON is what the assertions read, and it goes to
# its own file so a structured expectation parses a document rather than
# grepping prose.
#
# That split is not tidiness. The first version of this case grepped the plain
# report for "nothing to roll back to" and went red against a subject that was
# RIGHT: the row explaining why it was not saying that quotes the phrase inside
# the explanation. An assertion that cannot tell a claim from a discussion of
# the claim is not an assertion.
_surface() {
    local tag="$1"
    APEX_RECOVER_ROOT="$CASE_ROOT" "$APEX_BIN" recover status 2>&1
    local rc=$?
    APEX_RECOVER_ROOT="$CASE_ROOT" "$APEX_BIN" recover status --json \
        > "$CASE_DIR/$tag.json" 2>/dev/null || true
    printf '\n--- recover status --json is in %s.json ---\n' "$tag"
    return "$rc"
}

case_baseline() { _surface baseline || true; }

case_inject() {
    chmod 0000 "$DEPLOY_DIR"
    echo "chmod 0000 $DEPLOY_DIR"
}

case_prove() {
    # The independent observation. Not `chmod`'s exit code, and not a stat of
    # the mode bits either — the question is whether a READ is refused, and
    # only attempting one answers it.
    if ls "$DEPLOY_DIR" >/dev/null 2>&1; then
        echo "the deploy directory is still listable, so no fault was injected"
        return 1
    fi
    echo "listing $DEPLOY_DIR is refused: $(ls "$DEPLOY_DIR" 2>&1 | head -1)"
    return 0
}

case_observe() { _surface observe; }

# One expression, used three times, so the row being asserted about is named in
# one place.
ROW='[r for r in d["rows"] if r["id"]=="previous-deployment"][0]'
ROUTE='[r for r in d["routes"] if r["id"]=="previous-deployment"][0]'

case_judge() {
    # The baseline must have been the interesting state, or every assertion
    # below passes for the wrong reason. Two deployments -> available.
    expect_json "with the tree intact the rollback target is available" \
        "$CASE_DIR/baseline.json" "$ROW['state']" "available"

    # The claim itself: unknown, not absent. `unavailable` is the state that
    # means "nobody could measure this"; `attention` is the state the Ok(1)
    # arm uses for a machine that genuinely has one deployment. A subject that
    # answered `attention` here would be telling a user in trouble that their
    # way back does not exist.
    expect_json "an unreadable deploy directory is reported as unmeasured" \
        "$CASE_DIR/observe.json" "$ROW['state']" "unavailable"
    expect_json_nonempty "…and the detail names what could not be read" \
        "$CASE_DIR/observe.json" "$ROW['detail']"
    # The recovery-routes summary carries the same claim in a boolean, and it
    # is the one APEX Settings would grey a button on. `None` is "unknown";
    # `False` would be the hidden rollback.
    expect_json "with the tree intact the rollback route is available" \
        "$CASE_DIR/baseline.json" "$ROUTE['available']" "True"
    expect_json "the rollback route is unknown rather than unavailable" \
        "$CASE_DIR/observe.json" "$ROUTE['available']" "None"

    expect_differs_from_baseline "the surface reacted to the fault at all" \
        "$CASE_BASELINE_OUT" "$CASE_OBSERVED"

    # NOT asserted: that the exit code moves. `apex recover status` exits
    # non-zero on `Health::Attention` only, and deliberately not on
    # `Health::Unavailable` — every machine with no efivarfs, no GPU module
    # list, or a container's /proc has an unmeasurable row, and a status verb
    # that exited 1 on all of them would be useless as a check. The exit code
    # is recorded in the bundle as `observe.rc` so the decision stays visible;
    # changing it is a product decision about what APEX Settings polls, not
    # something this case may make by going red.

    # `apex recover status` is documented as spawning nothing and writing
    # nothing; under a fault is exactly when that must still hold. The one
    # expected change is the directory's own readability, which the snapshot
    # records as `dir:<unreadable>`.
    expect_no_corruption "reading a broken machine changed nothing on it" \
        "$CASE_DIR/state.before" "$CASE_DIR/state.after" \
        '^[<>] dir:<unreadable>'
}

# The directory must be readable again or the driver cannot snapshot the tree
# and the bundle is unreadable to a human afterwards. Restored by the case, not
# by a trap in the driver: a case that breaks the tree owns putting it back.
case_cleanup() { chmod 0755 "$DEPLOY_DIR" 2>/dev/null || true; }
