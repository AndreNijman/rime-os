#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-channel.sh — executable assertions for roadmap §26's update
#  channels and the health signal that stops a rollout.
#
#  Two modes, the split test-boot-v2.sh uses:
#
#    (no argument)     Structural checks with no toolchain: that `edge` is
#                      promoted with the tags it is a name for, that the three
#                      slower channels are NOT promoted on every build, that the
#                      read-back gate covers edge, and that the promotion
#                      dispatch verifies a signature and refuses to skip a
#                      channel. These live in `static` because they compare
#                      .github/workflows against rimed/, two different path
#                      selectors — a PR touching only the workflow sets
#                      rust=false, and the drift would ship.
#
#    --with-binary     Drives `rime channel` against fixture roots.
#
#  ── The claim these are guarding ────────────────────────────────────────────
#  `rime channel list` tells a user that stable carries "only builds that have
#  run on the other channels first". That sentence is true only if two things
#  hold in CI: the slower channels do not move on every build, and a promotion
#  refuses a digest that has not been on the channel above. Neither is visible
#  from the CLI, and both are one careless line away from being false while
#  every test still passes.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WITH_BINARY=0
[[ "${1:-}" == "--with-binary" ]] && WITH_BINARY=1

PASS=0 FAIL=0
ok()  { PASS=$((PASS + 1)); printf '  ok   %s\n' "$*"; }
bad() { FAIL=$((FAIL + 1)); printf '  FAIL %s\n' "$*"; }
sec() { printf '\n== %s ==\n' "$*"; }
has() {
    if grep -qF -- "$1" "$2"; then ok "$3"; else
        bad "$3 — no '$1' in:"; sed 's/^/       /' "$2" >&2
    fi
}
hasnt() {
    if grep -qF -- "$1" "$2"; then
        bad "$3 — found '$1' in:"; sed 's/^/       /' "$2" >&2
    else ok "$3"; fi
}

BUILD="$REPO/.github/workflows/build-image.yml"
PROMOTE="$REPO/.github/workflows/promote-channel.yml"
CHANNELRS="$REPO/rimed/rimed-core/src/channel.rs"
CLIRS="$REPO/rimed/rime/src/channel.rs"
OPSRS="$REPO/rimed/rime/src/ops.rs"
for f in "$BUILD" "$PROMOTE" "$CHANNELRS" "$CLIRS" "$OPSRS"; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done

TMP="$(mktemp -d)"
trap 'chmod -R u+rwX "$TMP" 2>/dev/null; rm -rf "$TMP"' EXIT

# ═════════════════════════════════════════════════════════════════════════════
sec "edge moves with the tags it is a name for"
promote_block="$(sed -n '/- name: Promote to every published tag/,/- name: Assert every published tag/p' "$BUILD")"
printf '%s\n' "$promote_block" > "$TMP/promote"
has 'promote edge' "$TMP/promote" "every build of main moves :edge"
# And the slow channels must NOT be here. A channel that advanced on every
# build is edge with a different spelling, and a user who chose it would be
# taking the same risk while believing they had opted out of it.
for c in stable candidate beta; do
    if grep -qE "promote \"?$c\"?\$" "$TMP/promote"; then
        bad ":$c is promoted on every build — it would be edge under another name"
    else
        ok ":$c is not promoted on every build"
    fi
done

sec "the read-back gate covers edge"
readback="$(sed -n '/- name: Assert every published tag resolves/,/exit 1/p' "$BUILD")"
printf '%s\n' "$readback" > "$TMP/readback"
has 'edge' "$TMP/readback" "the read-back loop includes edge"
# The four migration tags must still be there. They are what every machine in
# the field tracks, and a tag that stops moving does not error.
# `apex` is the one the rebrand could have dropped: machines installed before (rime-rename: keep)
# it track that tag by name. `rime` is the alias published beside it.
for t in apex rime daily gaming-mesa gaming-nvidia; do  # rime-rename: keep (apex is a tag machines track)
    has "$t" "$TMP/readback" "the read-back loop still includes $t"
done

sec "a promotion cannot point a channel at an unsigned or skipped build"
has 'cosign verify' "$PROMOTE" "the promotion verifies a signature"
has 'build-image\.yml@refs/heads/main$' "$PROMOTE" "against this repository's build workflow on main"
has 'already be on the channel above' "$PROMOTE" "a promotion refuses a build that skipped a channel"
for pair in 'beta)      above=edge' 'candidate) above=beta' 'stable)    above=candidate'; do
    has "$pair" "$PROMOTE" "the ladder step '$pair' is declared"
done
# workflow_dispatch only: a promotion that fired on a push would move stable on
# every merge, which is the failure this whole file exists to prevent.
if grep -qE '^  (push|pull_request|schedule):' "$PROMOTE"; then
    bad "promote-channel.yml fires on something other than a dispatch"
else
    ok "promote-channel.yml is workflow_dispatch only"
fi
group="$(sed -n 's/^  group: \([^ ]*\).*/\1/p' "$BUILD" | head -1)"
has "group: $group" "$PROMOTE" "it shares build-image.yml's concurrency group ($group)"

sec "the rollout stop is wired into the update path, not just available"
# A gate nobody's update consults is a report. `ops::update` is the only place
# that can refuse, so that is where the call has to be.
if grep -q 'channel::halt_reason' "$OPSRS"; then
    ok "ops::update consults the rollout stop"
else
    bad "nothing in ops.rs calls halt_reason — the stop cannot stop anything"
fi
if grep -q 'channel::record_update' "$OPSRS"; then
    ok "ops::update records what the machine was running"
else
    bad "nothing records the pre-update digest, so the gate can never arm"
fi
# The record is written BEFORE the pull. Written after a successful upgrade it
# would be missing for exactly the update that crashed the machine.
rec_line="$(grep -n 'channel::record_update' "$OPSRS" | head -1 | cut -d: -f1)"
# The pull is `crate::live::update(`: `bootc upgrade --download-only`, then the
# staged digest is verified again before it is queued for boot.
pull_line="$(grep -n 'crate::live::update(' "$OPSRS" | head -1 | cut -d: -f1)"
if [[ -n "$rec_line" && -n "$pull_line" && "$rec_line" -lt "$pull_line" ]]; then
    ok "the record is written before the pull (line $rec_line before $pull_line)"
else
    bad "the record is written after the pull ($rec_line vs $pull_line) — it would be missing for the update that broke the machine"
fi

sec "the health verdict cannot be built from a file that never exists"
# rime-boot-health is conditioned on systemd-boot's LoaderBootCountPath and
# every published image boots GRUB, so /var/lib/rime/boot/last-health.json has
# never been written on any Rime machine. A verdict that read it would be
# permanently empty and permanently green.
if grep -q 'last-health.json' "$CHANNELRS" "$CLIRS"; then
    bad "the health verdict reads last-health.json, which no published image writes"
else
    ok "the verdict does not depend on the boot-health file"
fi
if grep -q 'systemctl' "$CLIRS"; then
    ok "it uses failed units, which every boot path has"
else
    bad "nothing collects failed units"
fi

# ═════════════════════════════════════════════════════════════════════════════
if [[ "$WITH_BINARY" -eq 0 ]]; then
    printf '\n%s\n' "── binary checks skipped (pass --with-binary) ──"
    printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
    [[ "$FAIL" -eq 0 ]] || exit 1
    exit 0
fi

Rime="${Rime:-$REPO/rimed/target/debug/rime}"
[[ -x "$Rime" ]] || Rime="${CARGO_TARGET_DIR:-}/debug/rime"
if [[ ! -x "$Rime" ]]; then
    echo "FATAL: no rime binary. Build it, or set Rime=/path/to/rime." >&2
    echo "       A skipped assertion reports as a pass, which is the bug this refuses." >&2
    exit 1
fi

export XDG_CONFIG_HOME="$TMP/config"
export XDG_STATE_HOME="$TMP/state"
mkdir -p "$XDG_CONFIG_HOME/rime" "$XDG_STATE_HOME"

# A fixture machine: the ostree= symlink chain `rime trust` and `rime channel`
# both read, with a chosen tag in the deployment origin. $3, when given, is the
# booted digest a pre-rendered `rpm-ostree status --json` reports.
fixture() {
    local root="$TMP/$1" tag="$2" digest="${3:-}"
    local csum=f3f505fc39fb268c59f4458365c96b764a7bd7d30f2f51e98bb6a009666b7852
    local bootcsum=1d98b51dd76621b656c50e4f22dc7e5eade9b0f869443a3efa90eee08eb9373e
    rm -rf "$root"
    mkdir -p "$root/proc" "$root/etc/containers" \
             "$root/ostree/boot.0/default/$bootcsum" \
             "$root/ostree/deploy/default/deploy/$csum.0" \
             "$root/var/lib/rime/channel"
    printf 'root=UUID=x rw ostree=/ostree/boot.0/default/%s/0\n' "$bootcsum" > "$root/proc/cmdline"
    ln -sfn "../../../deploy/default/deploy/$csum.0" "$root/ostree/boot.0/default/$bootcsum/0"
    printf '[origin]\ncontainer-image-reference=ostree-unverified-registry:%s\n' "$tag" \
        > "$root/ostree/deploy/default/deploy/$csum.0.origin"
    printf '{"default":[{"type":"insecureAcceptAnything"}]}\n' > "$root/etc/containers/policy.json"
    # THE SECOND ROOT. `rime channel`'s health verdict is built from two
    # readers: `failed_units()`, which honours $RIME_TRUST_ROOT, and
    # `recover::health_rows()`, which is `Sys::from_env()` and honours
    # $RIME_RECOVER_ROOT. Setting only the first left the rollout stop half
    # fixture and half THIS MACHINE — and the half that came from the machine
    # is the `filesystem` row, which reads /proc/mounts.
    #
    # That is not theoretical. It is why `§26 channels` was red on the runner
    # and green on the laptop that wrote it: a GitHub runner mounts /usr
    # read-write, the row goes Attention, and "a healthy machine is not held"
    # measured the developer's own mount flags. Measured both ways with the
    # built binary before this line existed: same fixture, held=false against a
    # read-only /usr and held=true against a read-write one.
    #
    # A read-only /usr on an overlay root is what a Rime machine looks like.
    printf 'composefs / overlay ro,relatime 0 0\nnone /usr overlay ro,relatime 0 0\n' \
        > "$root/proc/mounts"
    if [[ -n "$digest" ]]; then
        # What `rpm-ostree status --json` would say. Under a fixture root the
        # binary reads this instead of spawning, which is the only reason the
        # rollout stop is reachable from a test at all.
        printf '{"deployments":[{"booted":true,"base-commit-meta":{"ostree.manifest-digest":"%s"}}]}\n' \
            "$digest" > "$root/rpm-ostree-status.json"
    fi
    printf '%s' "$root"
}

# The health verdict, driven through `systemctl --failed` — the one signal
# every boot path has, and the only one a fixture can set without rebuilding
# the whole recovery surface. The `rime recover status` rows are covered by
# rimed-core's own `verdict` tests, which feed each Health state directly.
break_unit()  { printf 'rime-shell.service loaded failed failed Rime Shell\n' > "$1/systemctl-failed"; }
repair_unit() { rm -f "$1/systemctl-failed"; }

record() {
    printf '{"schema":1,"from_digest":"%s","tag":"edge","at":1788700000}\n' "$2" \
        > "$1/var/lib/rime/channel/last-update.json"
}

held() { # held <trust-root> -> true / false / error:...
    RIME_TRUST_ROOT="$1" RIME_RECOVER_ROOT="$1" "$Rime" channel status --json \
        > "$TMP/held.json" 2>"$TMP/held.err" || true
    python3 -c 'import json,sys
try:
    print(str(json.load(open(sys.argv[1]))["held"]).lower())
except Exception as e:
    print("error:%s" % e)' "$TMP/held.json"
}

# `reasons` from the same fixture, one per line, so a case can say WHICH row
# held the machine rather than only that something did.
#
# The key is `health.reasons`, NESTED. `rime channel report --json` is the flat
# document with `reasons` at the top level (that is the payload the "exactly
# channel, tag, digest, healthy, reasons" case above pins); `status --json` is
# a different, larger document. Reading the flat path here printed
# `error:'reasons'` on every call — a diagnostic that diagnosed nothing, which
# is the exact defect class this file's own header is about. Asserted below
# rather than trusted: the helper is proven to return a real row name before
# any case is allowed to depend on it.
why() { # why <trust-root>
    RIME_TRUST_ROOT="$1" RIME_RECOVER_ROOT="$1" "$Rime" channel status --json \
        > "$TMP/why.json" 2>/dev/null || true
    python3 -c 'import json,sys
try:
    print("\n".join(json.load(open(sys.argv[1]))["health"]["reasons"]))
except Exception as e:
    print("error:%s" % e)' "$TMP/why.json" > "$TMP/why"
}

run() { RIME_TRUST_ROOT="$1" RIME_RECOVER_ROOT="$1" "$Rime" "${@:2}" > "$TMP/out" 2> "$TMP/err"; echo $?; }

sec "a machine installed before channels existed is told where it stands"
# The state of every Rime machine that exists. `:daily` moves on every build of
# main, so it IS edge, and "unknown channel" would be a worse answer than none.
R="$(fixture legacy 'ghcr.io/andrenijman/rime-os:daily')"
run "$R" channel status >/dev/null
has 'following    : daily' "$TMP/out" "the readout names the tag the machine follows"
has 'edge' "$TMP/out" "and says that tag is the edge channel"
has 'moves with every build' "$TMP/out" "and says why"
hasnt 'unknown' "$TMP/out" "it does not answer 'unknown'"

sec "a machine on a channel tag reads as that channel"
for c in stable candidate beta edge; do
    R="$(fixture "on-$c" "ghcr.io/andrenijman/rime-os:$c")"
    run "$R" channel status >/dev/null
    has "following    : $c" "$TMP/out" "a machine on :$c reads as $c"
done

sec "moving toward stable warns about the state that does not roll back"
R="$(fixture back 'ghcr.io/andrenijman/rime-os:edge')"
# --dry-run still needs root, and asserting the refusal is how we know the
# privileged classification did not drift to depend on the flag.
run "$R" channel set stable --dry-run >/dev/null
has 'must run as root' "$TMP/err" "channel set refuses without root, dry run included"

sec "the four channels are listed with what each one costs"
R="$(fixture list 'ghcr.io/andrenijman/rime-os:daily')"
run "$R" channel list >/dev/null
for c in stable candidate beta edge; do
    has "$c" "$TMP/out" "$c is listed"
done
has 'every successful build of main' "$TMP/out" "edge says what it costs"
has '* edge' "$TMP/out" "the machine's own channel is marked, through its alias"

sec "the report carries no identifier, and says nothing was sent"
R="$(fixture report 'ghcr.io/andrenijman/rime-os:daily')"
run "$R" channel report --json >/dev/null
python3 - "$TMP/out" <<'PY' && ok "the payload has exactly channel, tag, digest, healthy, reasons" \
    || bad "the payload's shape changed"
import json,sys
d=json.load(open(sys.argv[1]))
sys.exit(0 if sorted(d["wouldSend"]) == ["channel","digest","healthy","reasons","tag"] else 1)
PY
python3 - "$TMP/out" <<'PY' && ok "nothing was sent, and the JSON says so" || bad "the JSON does not record that nothing was sent"
import json,sys
d=json.load(open(sys.argv[1]))
sys.exit(0 if d["sent"] is False and d["optedIn"] is False and d["endpoint"] is None else 1)
PY
run "$R" channel report >/dev/null
has 'Nothing was sent' "$TMP/out" "the report says nothing was sent"
has 'reporting is off, which is the default' "$TMP/out" "and that off is the default"
hasnt 'machine-id' "$TMP/out" "the machine id is not in the payload"

sec "opting in without an endpoint still sends nothing, and says why"
printf 'report = true\n' > "$XDG_CONFIG_HOME/rime/channel.toml"
run "$R" channel report >/dev/null
has 'no endpoint is configured' "$TMP/out" "it names the missing endpoint"
has 'operates' "$TMP/out" "and says Rime runs no service to send it to"
rm -f "$XDG_CONFIG_HOME/rime/channel.toml"

sec "a channel nobody has heard of is refused by name"
R="$(fixture bogus 'ghcr.io/andrenijman/rime-os:daily')"
rc="$(run "$R" channel set nightly)"
[[ "$rc" != 0 ]] && ok "an invented channel exits non-zero" || bad "'nightly' was accepted"
for c in stable candidate beta edge; do
    has "$c" "$TMP/err" "the refusal names $c"
done

sec "the rollout stop fires, and only when it should"
# The assertion §26's second criterion rests on. Three mutation tests covered
# the channel model, the health verdict and the CI promotion, and not one of
# them covered the thing that actually stops a rollout: the stop was
# unreachable from a fixture until the record, the booted digest and the failed
# unit list all went through one. A gate nobody has watched fire is a gate
# nobody has tested.
OLD=sha256:5e206de5e00094276d73ef8ba85491b82573bd32e3e99a99597ee1266f81e677
NEW=sha256:308127d9cefeada90414ae37bdc8175d011c1f851ea9dde1661279a5da5bd89b

# 1. Rebooted into something new, and a unit that failed. This is the hold.
R="$(fixture stop 'ghcr.io/andrenijman/rime-os:edge' "$NEW")"
record "$R" "$OLD"
break_unit "$R"
case "$(held "$R")" in
    true)  ok "a machine that rebooted into a new image with a failed unit is held" ;;
    false) bad "the rollout stop did not fire on a regression" ;;
    *)     bad "rime channel status --json did not answer: $(cat "$TMP/held.err")" ;;
esac
RIME_TRUST_ROOT="$R" RIME_RECOVER_ROOT="$R" "$Rime" channel status > "$TMP/out" 2>&1 || true
has 'The next `rime update` is held' "$TMP/out" "the human readout says the next update is held"
has 'rime-shell.service' "$TMP/out" "and names the unit"

# `why()` is the diagnostic the case below reports with, so it is proven here
# against a machine that IS held — while a real reason exists to read. A helper
# that silently returns `error:...` would let the failure arm print a useless
# message at exactly the moment somebody needs it, and nothing would fail.
why "$R"
hasnt 'error:' "$TMP/why" "the reasons helper reads the document rather than erroring"
has 'rime-shell.service' "$TMP/why" "and it names the row that held the machine"

# 2. Same machine, same record, nothing failed. Nothing to hold.
repair_unit "$R"
case "$(held "$R")" in
    false) ok "a healthy machine is not held" ;;
    # Name the row. This arm went red on the runner for a DAY reporting only a
    # boolean, and the cause was the `filesystem` row reading the real
    # /proc/mounts — which the message could have said. A verdict a reader
    # cannot act on is most of the cost of a red step.
    true)  why "$R"
           bad "a healthy machine was held — every update would be refused; reasons: $(tr '\n' ';' < "$TMP/why")" ;;
    *)     bad "rime channel status --json did not answer: $(cat "$TMP/held.err")" ;;
esac

# 3. Broken, but it has NOT rebooted into the update: the record's digest is
#    what it is running. Nothing about the new image has been observed, so
#    holding would blame an update that never took effect.
R2="$(fixture notyet 'ghcr.io/andrenijman/rime-os:edge' "$OLD")"
record "$R2" "$OLD"
break_unit "$R2"
case "$(held "$R2")" in
    false) ok "a machine that has not rebooted into the update is not held" ;;
    true)  bad "held on an update that was never booted" ;;
    *)     bad "rime channel status --json did not answer: $(cat "$TMP/held.err")" ;;
esac

# 4. No record at all — a machine that has never run `rime update`.
R3="$(fixture norecord 'ghcr.io/andrenijman/rime-os:edge' "$NEW")"
break_unit "$R3"
case "$(held "$R3")" in
    false) ok "a machine with no update record is never held" ;;
    true)  bad "held a machine that has never updated" ;;
    *)     bad "rime channel status --json did not answer: $(cat "$TMP/held.err")" ;;
esac

# 5. The record is there, the unit failed, and the digest cannot be read. Every
#    uncertainty in this gate must permit: refusing somebody's update because a
#    file was unreadable strands them on the release that broke them.
R4="$(fixture nodigest 'ghcr.io/andrenijman/rime-os:edge')"
record "$R4" "$OLD"
break_unit "$R4"
case "$(held "$R4")" in
    false) ok "a digest that could not be read permits the update" ;;
    true)  bad "held on a digest nobody could read" ;;
    *)     bad "rime channel status --json did not answer: $(cat "$TMP/held.err")" ;;
esac

sec "the hold claims only what it measured"
# The record can be months old with an unrelated unit having failed yesterday.
# "This machine came back from its last update with a problem" asserts a cause
# nothing here established, on the one screen somebody reads while their
# machine is misbehaving.
break_unit "$R"
RIME_TRUST_ROOT="$R" RIME_RECOVER_ROOT="$R" "$Rime" channel status --json > "$TMP/out" 2>&1 || true
hasnt 'came back from its last update' "$TMP/out" "the JSON does not assert the update caused it"

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]] || exit 1
