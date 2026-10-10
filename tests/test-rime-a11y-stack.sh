#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-a11y-stack.sh — the image ships a screen reader, and nothing
#  starts it for the user (roadmap P2-003, "screen reader … validated").
#
#  ── The claim this corrects ─────────────────────────────────────────────────
#
#  It was on record that Rime shipped "ZERO accessibility packages". That was
#  derived by grepping Containerfiles, which only ever finds what is named
#  explicitly. Asked of the built image's OWN rpmdb instead —
#
#      rpm --dbpath <deploy>/usr/share/rpm -q at-spi2-core speech-dispatcher espeak-ng
#
#  — all three are installed, as transitive dependencies of gtk4 and Qt. The
#  plumbing and the speech engine were already there. The one missing piece was
#  the reader, which is a much smaller delta than the note suggested, and this
#  suite pins both halves so neither claim can drift again.
#
#  ── Why it does not grep for the word ───────────────────────────────────────
#
#  "orca" appears in Containerfile.core in prose as well as in the install line,
#  and a `grep -q orca` would stay green with the package removed and the
#  comment left behind — the exact shape (a grep over a stanza that also NAMES
#  the thing) that let mutant M7 survive earlier in this unit. So the package
#  list is EXTRACTED from the dnf5 invocations, comments discarded, and the
#  extractor is itself checked first against a FIXTURE whose right answer is
#  known — a real install line, a commented-out one, and a trailing-# comment on
#  a live line — and only then run over Containerfile.core. An earlier draft
#  used `ibus` (named only in this file's prose) as the negative control, and
#  that control could not fail: no comment here carries a dnf5 install line for
#  it to be wrongly read out of, so the assertion held however broken the parser
#  was. The fixture can fail, and does.
#
#  ── The other half: nothing starts it ───────────────────────────────────────
#
#  Shipping a screen reader is not the same as switching one on, and switching
#  one on for everybody would be a defect: a reader that starts unbidden talks
#  over a sighted user's first boot. The image autostarts fcitx5 in three
#  places; this asserts orca is in none of them, so "not autostarted" stays a
#  decision rather than becoming an accident either way.
#
#  ── What it will not do ─────────────────────────────────────────────────────
#
#  It starts nothing, opens no window, installs nothing and speaks. Reads files
#  in this repository, and the deployed image's rpmdb if there is one.
#
#  Run from anywhere: ./tests/test-rime-a11y-stack.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
set +e

cd "$(dirname "$0")" || exit 2
ROOT="$(cd .. && pwd)"
CORE="$ROOT/Containerfile.core"
[ -f "$CORE" ] || { echo "FATAL: cannot find $CORE" >&2; exit 2; }

pass=0; fail=0; skip=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s%s\n' "$1" "${2:+  — $2}"; fail=$((fail + 1)); }
skp()  { printf 'SKIP  %s%s\n' "$1" "${2:+  — $2}"; skip=$((skip + 1)); }
note() { printf 'NOTE  %s\n' "$1"; }
section() { printf '\n── %s ──\n' "$1"; }
is() {
    local name=$1 want=$2 got=$3
    if [ "$got" = "$want" ]; then ok "$name"
    else bad "$name" "want [$want] got [$got]"; fi
}
finish() {
    printf '\nrime-a11y-stack: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    [ "$fail" -eq 0 ]
}

# ═════════════════════════════════════════════════════════════════════════════
section "what Containerfile.core really installs"
# ═════════════════════════════════════════════════════════════════════════════

W="$(mktemp -d "${TMPDIR:-/tmp}/rime-a11y-stack.XXXXXX")" || exit 2
trap 'rm -rf "$W"' EXIT

# Every package name handed to a dnf5 install in a Containerfile, and nothing
# else: comments stripped first, continuations joined, options and shell
# operators discarded.
cat >"$W/extract.py" <<'PY'
import re, sys

text = open(sys.argv[1], errors='replace').read()
text = re.sub(r'\\\n', ' ', text)          # join RUN continuations
out = set()
for raw in text.splitlines():
    line = raw.strip()
    if line.startswith('#'):
        continue
    for chunk in re.split(r'[;&|]{1,2}', line):
        c = chunk.strip()
        m = re.search(r'\bdnf5?\b.*?\binstall\b(.*)$', c)
        if not m:
            continue
        for tok in m.group(1).split():
            if tok.startswith('#'):
                break
            if tok.startswith('-') or '=' in tok or tok == '\\':
                continue
            out.add(tok)
for p in sorted(out):
    print(p)
PY

# ── the extractor's own self-test, on a fixture built for it ────────────────
# "orca is installed" is a claim about this parser, so the parser is checked
# first against a file whose right answer is known — one real install line, one
# commented-out install line, and a trailing comment on a live line. All three
# shapes have to come out correctly or nothing below means anything.
cat >"$W/fixture.Containerfile" <<'FIX'
# An install line that is only prose:
#     dnf5 -y install ghost-from-a-comment
RUN set -eux; \
    dnf5 -y install real-one real-two \
        --setopt=install_weak_deps=False; \
    command -v real-one >/dev/null; \
    dnf5 clean all
RUN dnf5 -y install real-three   # ghost-after-a-hash
FIX
FIXOUT="$(python3 "$W/extract.py" "$W/fixture.Containerfile" | tr '\n' ' ')"
FIXOUT="${FIXOUT% }"
if [ "$FIXOUT" = "real-one real-three real-two" ]; then
    ok "the extractor takes exactly the packages a dnf5 install names"
else
    bad "the extractor takes exactly the packages a dnf5 install names" \
        "want [real-one real-three real-two] got [$FIXOUT]"
fi
if printf '%s\n' "$FIXOUT" | pipe_has 'ghost-from-a-comment'; then
    bad "a package named only in a comment is not extracted" \
        "the parser read a commented-out install line — this is the grep trap it exists to avoid"
else
    ok "a package named only in a comment is not extracted"
fi
if printf '%s\n' "$FIXOUT" | pipe_has 'ghost-after-a-hash'; then
    bad "a word after a trailing # on a live install line is not extracted" \
        "got [$FIXOUT]"
else
    ok "a word after a trailing # on a live install line is not extracted"
fi

PKGS="$(python3 "$W/extract.py" "$CORE")"

n_pkgs="$(printf '%s\n' "$PKGS" | grep -c .)"
if [ "$n_pkgs" -ge 50 ]; then
    ok "the package list was extracted from Containerfile.core ($n_pkgs names)"
else
    bad "the package list was extracted from Containerfile.core" \
        "only $n_pkgs names — the extractor found almost nothing, so every result below is meaningless"
    finish; exit 1
fi

if printf '%s\n' "$PKGS" | pipe_has -x 'fcitx5'; then
    ok "and it finds a package this file really does install (fcitx5)"
else
    bad "and it finds a package this file really does install (fcitx5)" \
        "fcitx5 is on a dnf5 install line here"
fi

if printf '%s\n' "$PKGS" | pipe_has -x 'orca'; then
    ok "Containerfile.core installs orca — the image ships a screen reader"
else
    bad "Containerfile.core installs orca — the image ships a screen reader" \
        "P2-003 names a screen reader in its acceptance line and there is none in the image"
fi

# ═════════════════════════════════════════════════════════════════════════════
section "and nothing starts it for the user"
# ═════════════════════════════════════════════════════════════════════════════
# A reader that starts unbidden talks over a sighted user's first boot. The
# three places this image really does autostart something are checked by name,
# so the assertion cannot pass by looking in the wrong files.

AUTOSTARTS="$ROOT/files/desktop/hypr/rime/session.lua
$ROOT/files/desktop/labwc/autostart
$ROOT/files/system/libexec/rime-hypr-migrate"

present=0; started=""
while IFS= read -r f; do
    [ -f "$f" ] || continue
    present=$((present + 1))
    grep -qE '(^|[^-[:alnum:]])orca([^-[:alnum:]]|$)' "$f" && started="$started $(basename "$f")"
done <<EOF
$AUTOSTARTS
EOF

if [ "$present" -eq 3 ]; then
    ok "all three of this image's autostart surfaces were found and read"
else
    bad "all three of this image's autostart surfaces were found and read" \
        "only $present of 3 exist; a renamed file would make the next assertion vacuous"
fi
# The positive control for the same read: fcitx5 IS autostarted in these files,
# so a grep that finds nothing at all is a broken grep rather than a clean bill.
control=0
while IFS= read -r f; do
    [ -f "$f" ] || continue
    grep -q 'fcitx5' "$f" && control=$((control + 1))
done <<EOF
$AUTOSTARTS
EOF
if [ "$control" -gt 0 ]; then
    ok "the same read does find the thing this image DOES autostart (fcitx5, in $control of them)"
else
    bad "the same read does find the thing this image DOES autostart (fcitx5)" \
        "if fcitx5 cannot be found here, 'orca is absent' means nothing"
fi
if [ -z "$started" ]; then
    ok "nothing in the image autostarts orca"
else
    bad "nothing in the image autostarts orca" \
        "started from:$started — a screen reader must be started by the user who wants it"
fi

# ═════════════════════════════════════════════════════════════════════════════
section "so there has to be a way to START it"
# ═════════════════════════════════════════════════════════════════════════════
# "Not autostarted" is only a defensible decision while the person who needs the
# reader can switch it on. Until /usr/libexec/rime-screen-reader there was no
# keybinding, no toggle and no menu entry, so the only route was to open a
# terminal and type `orca` — which is precisely the thing somebody who cannot
# see the screen cannot do first. A screen reader that cannot be launched by
# someone who needs a screen reader is not shipped.
#
# The script is RUN here, not read. Two facts it is built around came out of the
# rpm rather than out of memory, and both are invisible to a grep:
#
#   * orca 49.7 has no `--quit`/`-q`. The whole option list is
#     -h -v -r -s -l -e -d -p -u --speech-system --debug-file --debug. A toggle
#     written around `orca --quit` would have been able to turn the reader on
#     and never off, and would have looked perfectly correct in review.
#   * whether a running reader can be FOUND at all is the other half, and the
#     first version of this note got it wrong in a way worth keeping: it said
#     `pgrep -x orca` matches nothing because /usr/bin/orca is
#     `#!/usr/bin/python3`. Measured instead of assumed, Linux takes `comm`
#     from the SCRIPT's basename for a shebang script, so `comm` really is
#     `orca` and `-x` would have worked. The switch matches the command line
#     anyway, because that also finds a reader started as
#     `python3 /usr/bin/orca` — a superset, for a different reason than the one
#     first written down. The mutant for this row is that the probe stops
#     recognising the reader it started, which is the failure either spelling
#     can have.
#
# The stub below is strict about the option list (it refuses anything orca does
# not have) and its process really is stopped by a signal.

READER="$ROOT/files/system/libexec/rime-screen-reader"
if [ -f "$READER" ]; then
    ok "the image carries a screen-reader switch (files/system/libexec/rime-screen-reader)"
else
    bad "the image carries a screen-reader switch" \
        "there is no /usr/libexec/rime-screen-reader — orca ships with no way to start it"
fi

if [ -f "$READER" ] && bash -n "$READER" 2>"$W/reader.syntax"; then
    ok "the switch parses as bash"
else
    bad "the switch parses as bash" "$(head -2 "$W/reader.syntax" 2>/dev/null | tr '\n' ' ')"
fi

# Installed, not merely present in the tree. This is the K12/K13 shape: a
# launcher was once repointed at a path nothing installed, the ISO would have
# booted with no installer at all, and every source-reading assertion stayed
# green because the file plainly existed in the repo.
if grep -qE '^COPY .*files/system/libexec/rime-screen-reader[[:space:]]+/usr/libexec/rime-screen-reader$' "$CORE"; then
    ok "Containerfile.core installs it to /usr/libexec/rime-screen-reader"
else
    bad "Containerfile.core installs it to /usr/libexec/rime-screen-reader" \
        "nothing copies the switch into the image, so it exists only in this repository"
fi

# ── run it ──────────────────────────────────────────────────────────────────
if [ ! -f "$READER" ] || ! command -v pgrep >/dev/null 2>&1; then
    skp "the switch starts a reader, and pressing it again stops one" \
        "no script, or no pgrep here; COULD-NOT-RUN, not a pass"
else
    RB="$W/rbin"; mkdir -p "$RB"

    # A stub orca that is as strict about its command line as the real one's
    # argparse. `--quit` is not in orca 49's option list, so a switch that tried
    # to stop the reader that way would be refused here exactly as it is in
    # production — and the reader would still be running afterwards, which the
    # assertions below would see.
    cat >"$RB/orca" <<'EOF'
#!/bin/sh
for a in "$@"; do
    case "$a" in
        -r|--replace|-d|--disable|-e|--enable|-v|--version|-h|--help) ;;
        *) echo "orca: unrecognized arguments: $a" >&2; exit 2 ;;
    esac
done
# NOT `exec`: the point of this stub is to be found by a command-line probe, and
# an exec'd `sleep` has `sleep` in its argv and no trace of orca at all.
sleep 120
EOF
    # `systemctl` answers "this unit is unknown here", which is what a session
    # with no user manager looks like from inside the script, so this run
    # exercises the DIRECT path: setsid + SIGTERM.
    cat >"$RB/systemctl" <<'EOF'
#!/bin/sh
printf 'not-found\n'
exit 1
EOF
    chmod +x "$RB/orca" "$RB/systemctl"

    reader_state() {
        env PATH="$RB:/usr/bin:/bin" "$READER" status 2>/dev/null
    }
    reader_do() {
        env PATH="$RB:/usr/bin:/bin" "$READER" "$1" >/dev/null 2>&1
    }
    # The switch's own probe is scoped to this user, so the assertions have to
    # be too: a real orca belonging to somebody else on this machine must not
    # decide the verdict, and nothing here may signal it.
    stub_pids() { pgrep -u "$(id -u)" -f "$RB/orca" 2>/dev/null; }

    # Floor first. Every assertion below is about a state changing, and all of
    # them are vacuous if a reader was already running when the run started.
    if [ -z "$(stub_pids)" ]; then
        ok "no stub reader is running before the switch is touched"
    else
        bad "no stub reader is running before the switch is touched" \
            "a previous run leaked one; the assertions below would be meaningless"
    fi

    S0="$(reader_state)"
    reader_do toggle
    for _ in 1 2 3 4 5 6 7 8 9 10; do [ -n "$(stub_pids)" ] && break; sleep 0.2; done
    S1="$(reader_state)"
    PIDS_ON="$(stub_pids | tr '\n' ' ')"

    reader_do toggle
    for _ in 1 2 3 4 5 6 7 8 9 10; do [ -z "$(stub_pids)" ] && break; sleep 0.2; done
    S2="$(reader_state)"
    PIDS_OFF="$(stub_pids | tr '\n' ' ')"

    is "with no reader running, the switch reports off" "off" "$S0"
    if [ -n "$PIDS_ON" ]; then
        ok "one press starts a reader (pid$( [ "$(printf '%s' "$PIDS_ON" | wc -w)" -gt 1 ] && printf 's') $PIDS_ON)"
    else
        bad "one press starts a reader" \
            "nothing was started — a switch that cannot turn the reader ON is the whole defect"
    fi
    is "and the switch then reports on" "on" "$S1"
    if [ -z "$PIDS_OFF" ]; then
        ok "a second press stops it"
    else
        bad "a second press stops it" \
            "still running as $PIDS_OFF — orca has no --quit verb, so the switch has to signal it"
    fi
    is "and the switch reports off again" "off" "$S2"

    # Belt and braces: never leave a process behind whatever the assertions said.
    stub_pids | xargs -r kill -TERM 2>/dev/null

    # ── the systemd path ────────────────────────────────────────────────────
    # orca 49 ships /usr/lib/systemd/user/orca.service — Restart=always,
    # WatchdogSec=6 — and a reader that dies silently is worse than one that
    # never started, because the user cannot see that it stopped. So the unit is
    # preferred whenever the user manager answers. That preference is a real
    # branch and it needs its own evidence: with a systemctl that DOES know the
    # unit, the switch must go through it rather than exec'ing orca behind
    # systemd's back.
    cat >"$RB/systemctl" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >>"$W/systemctl.calls"
case "\$*" in
    *'show -p LoadState'*) printf 'loaded\n'; exit 0 ;;
    *start*) printf '%s\n' start >>"$W/systemctl.verbs"; exit 0 ;;
    *stop*)  printf '%s\n' stop  >>"$W/systemctl.verbs"; exit 0 ;;
esac
exit 0
EOF
    chmod +x "$RB/systemctl"
    : >"$W/systemctl.verbs"
    reader_do on
    if grep -qx start "$W/systemctl.verbs" 2>/dev/null; then
        ok "where the user manager knows orca.service, the switch starts the UNIT"
    else
        bad "where the user manager knows orca.service, the switch starts the UNIT" \
            "it went straight to orca, so a reader that crashes never comes back"
    fi
    if [ -z "$(stub_pids)" ]; then
        ok "and does not also exec a second reader behind systemd's back"
    else
        stub_pids | xargs -r kill -TERM 2>/dev/null
        bad "and does not also exec a second reader behind systemd's back" \
            "both paths ran; two readers speak over each other"
    fi
fi

# ═════════════════════════════════════════════════════════════════════════════
section "the plumbing was already there — asked of the image, not of a grep"
# ═════════════════════════════════════════════════════════════════════════════

DEPLOY="$(ls -d /ostree/deploy/*/deploy/*/ 2>/dev/null | head -1)"
DB=""
[ -n "$DEPLOY" ] && [ -d "$DEPLOY/usr/share/rpm" ] && DB="$DEPLOY/usr/share/rpm"

if [ -z "$DB" ]; then
    skp "the built image carries at-spi2-core, speech-dispatcher and espeak-ng" \
        "no ostree deployment on this machine to ask; COULD-NOT-RUN, not a pass"
elif ! rpm --dbpath "$DB" -q rpm >/dev/null 2>&1; then
    skp "the built image carries at-spi2-core, speech-dispatcher and espeak-ng" \
        "the deployment's rpmdb at $DB could not be read (permission denied is not absence)"
else
    for p in at-spi2-core at-spi2-atk speech-dispatcher espeak-ng; do
        if rpm --dbpath "$DB" -q "$p" >/dev/null 2>&1; then
            ok "the built image carries $p ($(rpm --dbpath "$DB" -q "$p"))"
        else
            bad "the built image carries $p" \
                "it used to arrive as a transitive dependency of gtk4/Qt; if it is gone, orca alone is not a working reader"
        fi
    done

    # Deliberately a NOTE. This machine's deployment may predate the stanza
    # above, and there is no way from here to tell "built before the change"
    # apart from "the change regressed" — so it is reported and not asserted.
    if rpm --dbpath "$DB" -q orca >/dev/null 2>&1; then
        note "the running deployment already carries $(rpm --dbpath "$DB" -q orca)"
    else
        note "the running deployment has no orca — expected until an image built from this tree is deployed"
        note "  (the assertion that the SOURCE installs it is above, and it is the one that gates the change)"
    fi
fi

finish
