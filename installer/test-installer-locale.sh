#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-installer-locale.sh — the keyboard, locale and timezone the installer
#  writes into the installed system (roadmap P2-004, "keyboard layout before
#  password creation").
#
#  ── The defect ──────────────────────────────────────────────────────────────
#
#  Every Rime install lands on `us` / `en_US.UTF-8` / `Australia/Perth`.
#
#  Not by choice — there was nowhere to make one. The GUI's ten pages
#  (welcome → wifi → disk → mode → part → account → secureboot → confirm → run
#  → done) had no locale, keyboard or timezone step, and the engine's
#  `set_locale_keymap_in()` only COPIES whatever the live ISO already resolved.
#  The live ISO's values come from a kickstart that hardcodes exactly those
#  three (`installer/bib-config.toml`).
#
#  So a user in Germany installs Rime, is asked to create a password, types it
#  on a keyboard the installer has decided is American, and then cannot log in
#  to the machine they just installed — the same lockout the greeter's keymap
#  generator, sway-greet.conf and labwc-greet/environment each carry a note
#  about, arriving one step earlier in the story.
#
#  ── Why it extracts rather than restates ────────────────────────────────────
#
#  `set_locale_keymap_in()` cannot be called directly: it is halfway through an
#  engine that refuses to run without a block device, and everything before it
#  would erase one. So the function is LIFTED OUT OF THE SHIPPED ENGINE with
#  sed and run against a fake deploy tree — the pattern test-installer.sh
#  already uses for `scratch_fs_ok`/`pick_scratch`, whose comment says it tests
#  "what installs, not a paraphrase of it".
#
#  The extraction is checked for plausibility before it is used: an extraction
#  that silently produced an empty function would make every assertion below
#  pass for no reason, so a short or shapeless one is a FAILURE, never a skip.
#
#  ── What it will not do ─────────────────────────────────────────────────────
#
#  It touches no block device, runs no installer, and needs no root. Everything
#  happens inside a mktemp deploy tree, and `localectl`, `timedatectl` and
#  `setfiles` are stubbed on a private PATH so the function cannot read or
#  change this machine.
#
#  Run from anywhere: ./installer/test-installer-locale.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# Deliberately +e, like every suite in this tree: CI invokes a suite as
# `bash -e {0}`, and under -e an assignment from a failing command ends the run
# silently, mid-section.
set +e

cd "$(dirname "$0")" || exit 2
ENGINE="$PWD/rime-install"
GUI="$PWD/rime-installer-gui"
for f in "$ENGINE" "$GUI"; do
    [ -f "$f" ] || { echo "FATAL: cannot find $f" >&2; exit 2; }
done

pass=0; fail=0; skip=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s%s\n' "$1" "${2:+  — $2}"; fail=$((fail + 1)); }
skp() { printf 'SKIP  %s%s\n' "$1" "${2:+  — $2}"; skip=$((skip + 1)); }
section() { printf '\n── %s ──\n' "$1"; }
is() {
    local name=$1 want=$2 got=$3
    if [ "$got" = "$want" ]; then ok "$name"
    else bad "$name" "want [$want] got [$got]"; fi
}
finish() {
    printf '\ninstaller-locale: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    [ "$fail" -eq 0 ]
}

W="$(mktemp -d "${TMPDIR:-/tmp}/rime-inst-locale.XXXXXX")" || exit 2
# The engine refuses to start without bootc (it installs with the live env's
# own), and the GitHub runner this suite runs on has none. Every engine case
# here is a dry run that stops before bootc would run, so a stub that is never
# executed is all preflight needs; if one ever IS executed, it fails loudly
# rather than pretending to have installed anything. RIME_BOOTC is the engine's
# seam for exactly this, and sudo's env_reset means it is passed per call.
BOOTC_STUB="$W/bootc-stub"
printf '#!/bin/sh\necho "bootc stub executed by a dry run: $*" >&2\nexit 99\n' > "$BOOTC_STUB"
chmod 755 "$BOOTC_STUB"
cleanup() { rm -rf "$W"; }
trap cleanup EXIT INT TERM

# ── The page order, read out of the GUI ──────────────────────────────────────
# The criterion is "keyboard layout before password CREATION", and the password
# is created on the `account` page. That is an ORDER claim, so it is checked as
# one — and against the navigation graph, not the registry: the registry is an
# unordered dict, and a page present in it but unreachable from welcome would
# satisfy a membership test while never appearing.
section "the keyboard step comes before the password is created"

python3 - "$GUI" > "$W/order.txt" 2>/dev/null <<'PY'
import re, sys
src = open(sys.argv[1]).read()
# Each page builder is `def p_<name>(self):` and its forward links are the
# self.go("...") calls inside it. Walk from welcome and print the reachable
# order, depth-first along the first forward edge that is not a back link.
bodies = {}
for m in re.finditer(r'\n    def p_(\w+)\(self\).*?(?=\n    def |\Z)', src, re.S):
    bodies[m.group(1)] = m.group(0)
edges = {}
for name, body in bodies.items():
    # Back buttons are edges too, and following them makes the graph almost
    # undirected: with `Begin` pointing straight at wifi, `keyboard` was still
    # "reachable" — through wifi's own Back button — and still landed before
    # `account` in the walk. A mutation proved it: moving the keyboard page out
    # of the forward flow entirely changed no assertion. So drop the reverse
    # edges and walk only the way a user actually goes.
    body = re.sub(r'self\.btn\(\s*"Back".*?\)\s*,\s*False\)', '', body, flags=re.S)
    targets = re.findall(r'self\.go\(\s*"(\w+)"', body)
    targets += re.findall(r'go\(\s*"(\w+)"\s*\)', body)
    # Ternary forms: self.go("a" if cond else "b")
    for a, b in re.findall(r'self\.go\(\s*"(\w+)"\s+if\s+.*?\s+else\s+"(\w+)"\s*\)', body):
        targets += [a, b]
    edges[name] = targets
seen, order, stack = set(), [], ["welcome"]
while stack:
    n = stack.pop(0)
    if n in seen or n not in edges:
        continue
    seen.add(n); order.append(n)
    for t in edges[n]:
        if t not in seen:
            stack.append(t)
print(" ".join(order))
PY
ORDER="$(cat "$W/order.txt")"

if [ -z "$ORDER" ]; then
    bad "the page graph was extracted from the GUI" "extraction produced nothing"
    finish; exit 1
fi
ok "the page graph was extracted from the GUI"
echo "      reachable from welcome: $ORDER"

# The premise: if `account` is not in the graph the extraction is wrong and
# every ordering claim below is vacuous.
case " $ORDER " in
    *" account "*) ok "the account page is reachable from welcome" ;;
    *) bad "the account page is reachable from welcome" "extraction is wrong; the order assertions below would be vacuous"
       finish; exit 1 ;;
esac

pos_of() {   # pos_of <page>
    local i=0 p
    for p in $ORDER; do
        i=$((i + 1))
        [ "$p" = "$1" ] && { printf '%s' "$i"; return 0; }
    done
    printf '0'
}

kb_pos=$(pos_of keyboard)
acct_pos=$(pos_of account)
if [ "$kb_pos" -eq 0 ]; then
    bad "there is a keyboard page at all" \
        "no page named 'keyboard' is reachable from welcome — a user cannot choose a layout before typing a password"
elif [ "$kb_pos" -lt "$acct_pos" ]; then
    ok "the keyboard page comes before the account page ($kb_pos < $acct_pos)"
else
    bad "the keyboard page comes before the account page" \
        "keyboard is step $kb_pos, account is step $acct_pos"
fi

# The ordering above is a property of the whole graph; this is the single edge
# the criterion actually rests on. Stated separately because a graph assertion
# can stay green while the one step that matters moves — which is exactly what
# the mutation showed.
prim="$(python3 - "$GUI" <<'PY2' 2>/dev/null
import re, sys
src = open(sys.argv[1]).read()
m = re.search(r'\n    def p_welcome\(self\).*?(?=\n    def |\Z)', src, re.S)
body = m.group(0) if m else ""
# The primary action is the rime-go button; "Back"/"Quit" are rime-ghost.
g = re.search(r'self\.btn\(\s*"[^"]*"\s*,\s*"rime-go"\s*,\s*lambda[^)]*self\.go\(\s*"(\w+)"', body)
print(g.group(1) if g else "")
PY2
)"
if [ "$prim" = "keyboard" ]; then
    ok "the welcome page's primary button leads straight to the keyboard page"
else
    bad "the welcome page's primary button leads straight to the keyboard page" \
        "it leads to '${prim:-<not found>}' — the keyboard step is not in the forward flow"
fi

# test-installer.sh's render suite extracts page names with grep -oE '"[a-z]+"'.
# A page named keyboard-locale or kb_tz is silently DROPPED from it with no
# failure at all, so the name is asserted here rather than discovered later.
if printf '%s' "$ORDER" | tr ' ' '\n' | grep -qx 'keyboard'; then
    ok "the page's name survives test-installer.sh's [a-z]+ page-name extraction"
else
    skp "the page's name survives test-installer.sh's [a-z]+ extraction" "no keyboard page"
fi

# ── The engine half ──────────────────────────────────────────────────────────
section "the engine honours the operator's choice"

FN="$W/fn.sh"
sed -n '/^set_locale_keymap_in()/,/^}/p' "$ENGINE" > "$FN"
# ── the function's OWN dependencies, or it measures their absence ────────────
#
# set_locale_keymap_in() calls console_keymap_for(), which calls kbd_has().
# Extracting only the first left both undefined, so the call substituted to
# EMPTY and the engine wrote a blank `KEYMAP=` — which this suite read as the
# engine failing to resolve `de`. It was not: given its dependencies the same
# engine writes `KEYMAP=de`. The suite was measuring its own extraction.
#
# Invisible for a second reason worth recording: pr-validation's `changes`
# selector leaves `installer=false` on a push to an integration branch, and a
# SKIPPED job reports success. This job therefore only ever runs on a
# pull_request — the merge to main, where a red is most expensive.
sed -n '/^console_keymap_for()/,/^}/p' "$ENGINE" >> "$FN"
sed -n '/^kbd_has()/,/^}/p'            "$ENGINE" >> "$FN"
for _dep in console_keymap_for kbd_has; do
    grep -q "^${_dep}()" "$FN" \
        || die "the engine no longer defines ${_dep}(); this suite extracts it and would otherwise silently measure nothing"
done
# ── Take the HOST'S CLOCK out of the measurement ─────────────────────────────
# The function infers a timezone from the real /etc/localtime. That made three
# assertions below depend on the machine running them: this passed on a laptop
# set to Australia/Perth and FAILED in CI, where a GitHub runner resolves
# Etc/UTC. A test whose verdict depends on where the tester lives is not
# measuring the engine.
#
# So the one absolute path the function reads is redirected to a file this suite
# controls. The substitution is asserted below rather than assumed, because a
# silent miss would restore exactly the host-dependence it exists to remove.
#
# Only the two HOST reads are redirected. A blanket s|/etc/localtime|…| also
# rewrites "$deploy/etc/localtime", which is where the function WRITES its
# result — the first attempt did exactly that, and every timezone assertion
# went red with an empty value because the link was being created in the fake
# host directory instead of the deploy root. Both halves are asserted below.
FAKE_ETC="$W/fake-etc"
mkdir -p "$FAKE_ETC"
sed -i -e "s|-L /etc/localtime|-L $FAKE_ETC/localtime|g" \
       -e "s|readlink /etc/localtime|readlink $FAKE_ETC/localtime|g" "$FN"
n_lines=$(wc -l < "$FN")
if [ "$n_lines" -lt 30 ]; then
    bad "set_locale_keymap_in extracted from the engine" "only $n_lines lines; the extraction is wrong"
    finish; exit 1
fi
ok "set_locale_keymap_in extracted from the engine ($n_lines lines)"

# The redirect has to have landed, or every timezone assertion silently goes
# back to reading the tester's own machine.
if grep -q "readlink $FAKE_ETC/localtime" "$FN"; then
    ok "the extracted copy READS a timezone this suite controls, not the host's"
else
    bad "the extracted copy READS a timezone this suite controls, not the host's" \
        "the redirect did not apply; the assertions below would measure this machine"
fi
# The other half, and not a formality: over-substituting here silently moves the
# function's OUTPUT out of the deploy root, which turns every assertion below
# into "want [a zone] got []".
if grep -q 'deploy/etc/localtime' "$FN"; then
    ok "…while still WRITING its result into the deploy root"
else
    bad "…while still WRITING its result into the deploy root" \
        "the deploy path was rewritten too; the assertions below would all read empty"
fi

# Stubs. The function reads the LIVE machine through localectl/timedatectl and
# relabels through setfiles; none of that may happen here, and a stub that
# answered would make the override assertions meaningless.
mkdir -p "$W/bin"
for t in localectl timedatectl setfiles; do
    printf '#!/usr/bin/env bash\nexit 1\n' > "$W/bin/$t"
    chmod +x "$W/bin/$t"
done

# host_tz <zone|"">  — what the "live environment" resolves, for this run only.
host_tz() {
    rm -f "$FAKE_ETC/localtime"
    [ -n "${1:-}" ] && ln -sfn "../usr/share/zoneinfo/$1" "$FAKE_ETC/localtime"
}

run_fn() {   # run_fn <deploy> [KEYMAP] [KEYVARIANT] [TIMEZONE]
    local deploy="$1" km="${2:-}" kv="${3:-}" tz="${4:-}"
    PATH="$W/bin:$PATH" \
    KEYMAP="$km" KEYVARIANT="$kv" TIMEZONE="$tz" \
    LOG="$W/engine.log" DEPLOY="$deploy" \
    bash -c '
        set +u
        have() { command -v "$1" >/dev/null 2>&1; }
        log()  { printf "%s\n" "$*" >> "$LOG"; }
        . '"$FN"'
        set_locale_keymap_in "$DEPLOY"
    ' >/dev/null 2>&1
}

mk_deploy() {   # mk_deploy -> prints a fresh fake deploy root
    local d; d="$(mktemp -d "$W/deploy.XXXXXX")"
    mkdir -p "$d/etc" "$d/usr/share/zoneinfo/Europe" "$d/usr/share/zoneinfo/Australia"
    # A keymap tree IN THE FIXTURE. console_keymap_for() prefers the target's
    # own data and falls back to the HOST's /usr/lib/kbd/keymaps — so without
    # this the verdict depends on whether the machine running the suite has kbd
    # installed, the same class of defect the localtime redirection above
    # exists to remove. Proven hermetic by mutation: delete de.map.gz and the
    # case answers `us`, not `de`, so the fixture is what is consulted.
    mkdir -p "$d/usr/lib/kbd/keymaps/xkb"
    : > "$d/usr/lib/kbd/keymaps/xkb/de.map.gz"
    : > "$d/usr/lib/kbd/keymaps/xkb/us.map.gz"
    : > "$d/usr/share/zoneinfo/Europe/Berlin"
    : > "$d/usr/share/zoneinfo/Australia/Perth"
    printf '%s' "$d"
}

# (1) An explicit German layout must reach all three files.
D1="$(mk_deploy)"
run_fn "$D1" de "" ""
got="$(sed -n 's/.*Option "XkbLayout" "\([^"]*\)".*/\1/p' "$D1/etc/X11/xorg.conf.d/00-keyboard.conf" 2>/dev/null)"
is "an explicit layout reaches the X11 keyboard config" "de" "$got"
got="$(sed -n 's/^KEYMAP=//p' "$D1/etc/vconsole.conf" 2>/dev/null)"
is "…and the console keymap" "de" "$got"

# (2) A variant travels with it. A German user on the `nodeadkeys` variant who
#     gets plain `de` still has a different keyboard than the one they chose.
D2="$(mk_deploy)"
run_fn "$D2" de nodeadkeys ""
got="$(sed -n 's/.*Option "XkbVariant" "\([^"]*\)".*/\1/p' "$D2/etc/X11/xorg.conf.d/00-keyboard.conf" 2>/dev/null)"
is "an explicit variant reaches the X11 keyboard config" "nodeadkeys" "$got"

# (3) THE REGRESSION THIS GUARDS. With no choice the function must behave
#     exactly as it always has — fall back to `us` when the live environment
#     cannot be read — rather than writing an empty layout, which is a keyboard
#     config that configures nothing.
D3="$(mk_deploy)"
run_fn "$D3" "" "" ""
got="$(sed -n 's/.*Option "XkbLayout" "\([^"]*\)".*/\1/p' "$D3/etc/X11/xorg.conf.d/00-keyboard.conf" 2>/dev/null)"
is "no choice still falls back to us, as it always did" "us" "$got"

# (4) A chosen timezone becomes the symlink.
D4="$(mk_deploy)"
run_fn "$D4" "" "" "Europe/Berlin"
got="$(readlink "$D4/etc/localtime" 2>/dev/null | sed 's|.*/zoneinfo/||')"
is "an explicit timezone becomes /etc/localtime" "Europe/Berlin" "$got"

# (5) An operator who explicitly picks UTC means UTC. The pre-existing
#     ""|"UTC" -> Australia/Perth fallback exists to replace systemd's INFERRED
#     UTC, and must not overrule a deliberate one.
D5="$(mk_deploy)"
mkdir -p "$D5/usr/share/zoneinfo"; : > "$D5/usr/share/zoneinfo/UTC"
run_fn "$D5" "" "" "UTC"
got="$(readlink "$D5/etc/localtime" 2>/dev/null | sed 's|.*/zoneinfo/||')"
is "an explicitly chosen UTC is not overruled by the Perth fallback" "UTC" "$got"

# (6) No timezone chosen, and a live environment with no zone set: the
#     long-standing Australia/Perth fallback, which is the value the ISO's own
#     kickstart uses so the two install paths agree.
host_tz ""
D6="$(mk_deploy)"
run_fn "$D6" "" "" ""
got="$(readlink "$D6/etc/localtime" 2>/dev/null | sed 's|.*/zoneinfo/||')"
is "no choice and no live zone falls back to Australia/Perth" "Australia/Perth" "$got"

# (7) THE CI FINDING. systemd writes the CANONICAL name, and on a machine with
#     no timezone configured that name is "Etc/UTC" — not "UTC". The engine's
#     guard matched only ""|"UTC", so the commonest spelling of the exact case
#     it exists to catch went straight past it: the deploy root had no Etc/UTC
#     in it and the installed system got NO /etc/localtime at all.
host_tz "Etc/UTC"
D7="$(mk_deploy)"
run_fn "$D7" "" "" ""
got="$(readlink "$D7/etc/localtime" 2>/dev/null | sed 's|.*/zoneinfo/||')"
is "a live environment on Etc/UTC is treated as unset, not copied through" \
   "Australia/Perth" "$got"

# (8) …and a live environment with a REAL zone is carried over untouched. This
#     is the other half: (7) must not become "always Perth", which would throw
#     away a correct value the ISO had already resolved.
host_tz "Europe/Berlin"
D8="$(mk_deploy)"
run_fn "$D8" "" "" ""
got="$(readlink "$D8/etc/localtime" 2>/dev/null | sed 's|.*/zoneinfo/||')"
is "a live environment on a real zone is carried into the install" \
   "Europe/Berlin" "$got"

# Restore a neutral host zone for anything after this point.
host_tz ""

# ── The answers file ─────────────────────────────────────────────────────────
# The engine dies on an unknown key, so a GUI that writes one the engine does
# not know breaks EVERY install. The two halves have to land together, and this
# is the assertion that says they did.
section "the answers file carries the new keys"
for k in keymap keyvariant timezone; do
    if grep -qE "^\s+$k\)" "$ENGINE"; then
        ok "the engine accepts '$k' in the answers file"
    else
        bad "the engine accepts '$k' in the answers file" \
            "unknown keys are fatal; the GUI writing this would break every install"
    fi
done

# Cleared before parsing, for the reason the engine's own comment gives: a stale
# value reaching `bootc install to-disk --wipe` is how this file already lost a
# real USB stick once.
for v in KEYMAP KEYVARIANT TIMEZONE; do
    if grep -qE "^KEYMAP=\"\"|^$v=\"\"|; $v=\"\"" "$ENGINE"; then
        ok "$v is cleared before the answers file is parsed"
    else
        bad "$v is cleared before the answers file is parsed"
    fi
done

# ── Validation happens before the disk is touched ────────────────────────────
# The engine's own comment: an invalid value is only caught when it is USED, and
# by then `bootc install --wipe` has already run. A bad layout must be refused
# with the same "Nothing has been erased." promise the account checks make.
#
# ── Getting the engine as far as its own validation ─────────────────────────
#
# rime-install:353 refuses to go on unless the Rime OS image is in ROOT podman
# storage, and that check sits BEFORE argument parsing. So on any box that is
# not the ISO build box the engine dies at preflight and never reaches a single
# answers-file guard. (This is not hypothetical: test-installer.sh's whole
# engine half has been dead for exactly this reason since the cases were added
# five days after the check — including in CI, on a bare ubuntu runner.)
#
# rime-install:56 is `IMAGE="${RIME_IMAGE:-localhost/rime-os:${EDITION}}"`, with
# the comment "override with RIME_IMAGE=... for testing". So point it at an
# image that does exist. An empty tar imported by podman is a valid image with
# no layers, costs nothing, needs no network, and is removed again on exit — so
# the suite does not depend on whatever happens to be in the ambient store.
#
# Checked before use rather than assumed safe: the only `bootc install to-disk
# --wipe` reachable before the validation block is inside `if [ "$UNATTENDED" =
# 1 ]`, whose two gates (`rime.unattended` on /proc/cmdline, and
# /usr/share/rime-installer/allow-unattended) are both shut on a developer box.
# Everything else between preflight and validation is function definitions. The
# disk named below cannot exist, so a value that PASSES validation stops at the
# block-device check with nothing touched.
SCRATCH_IMAGE="localhost/rime-locale-probe:test"
ENGINE_IMAGE=""
scratch_made=0
drop_scratch() {
    [ "$scratch_made" = 1 ] && sudo -n podman rmi -f "$SCRATCH_IMAGE" >/dev/null 2>&1
    scratch_made=0
}
cleanup() { drop_scratch; rm -rf "$W"; }

ensure_engine_image() {
    command -v sudo   >/dev/null 2>&1 || return 1
    command -v podman >/dev/null 2>&1 || return 1
    sudo -n true 2>/dev/null || return 1
    # Note: sudo's env_reset strips RIME_* from the caller's environment, so the
    # override has to be passed as `sudo -n RIME_IMAGE=... ` and cannot be
    # exported here.
    if sudo -n podman image exists localhost/rime-os:daily 2>/dev/null; then
        ENGINE_IMAGE="localhost/rime-os:daily"; return 0
    fi
    tar -cf "$W/empty.tar" -T /dev/null 2>/dev/null || return 1
    sudo -n podman import -q "$W/empty.tar" "$SCRATCH_IMAGE" >/dev/null 2>&1 || return 1
    scratch_made=1
    ENGINE_IMAGE="$SCRATCH_IMAGE"
    return 0
}

engine() {   # engine <answers-file> -> the engine's combined output
    sudo -n RIME_BOOTC="$BOOTC_STUB" RIME_DRY_RUN=1 RIME_IMAGE="$ENGINE_IMAGE" "$ENGINE" --headless "$1" 2>&1 </dev/null
}

section "a bad layout is refused before anything is erased"
if ! ensure_engine_image; then
    skp "a nonsense layout is refused" \
        "needs passwordless sudo + podman to put an image where preflight can find one"
    skp "a real layout gets past validation" "same"
else
    ok "the engine can reach its own validation (RIME_IMAGE=$ENGINE_IMAGE)"
    ANS="$W/answers"
    printf 'mode=disk\ndisk=/dev/zzz-does-not-exist\nusername=u\npassword=pw\nhostname=rime\nencrypt=no\nkeymap=NOT_A_LAYOUT\n' > "$ANS"
    out="$(engine "$ANS")"
    if grep -q 'Unexpected error on line' <<<"$out"; then
        bad "a nonsense layout is refused" "the ERR trap fired instead of a clean refusal"
    elif grep -qF "Nothing has been erased" <<<"$out" && grep -qiF "layout" <<<"$out"; then
        ok "a nonsense layout is refused, naming the layout, before the disk is touched"
    else
        bad "a nonsense layout is refused" "got: $(printf '%s' "$out" | tail -2 | tr '\n' ' ')"
    fi

    # And a real one must NOT be refused for being a layout — it has to get past
    # this check and fail later on the absent disk, or the validator is simply
    # rejecting everything and assertion (1) above proves nothing.
    printf 'mode=disk\ndisk=/dev/zzz-does-not-exist\nusername=u\npassword=pw\nhostname=rime\nencrypt=no\nkeymap=de\n' > "$ANS"
    out="$(engine "$ANS")"
    if grep -qiF "is not a keyboard layout" <<<"$out"; then
        bad "a real layout is not refused" "the validator rejects valid layouts too"
    elif grep -qF "is not a block device" <<<"$out"; then
        # Named exactly: it must die at the BLOCK DEVICE check, which is the
        # guard immediately after validation. Any other death would mean the
        # layout got past validation for some unrelated reason.
        ok "a real layout gets past validation and stops at the absent disk"
    else
        bad "a real layout gets past validation and stops at the absent disk" \
            "got: $(grep -m1 RIME-INSTALL-FAILED <<<"$out" || echo '<no sentinel>')"
    fi
fi

finish
